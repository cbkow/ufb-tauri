#!/bin/bash
# bundle-macos-deps.sh
#
# Copies FFmpeg dylibs, binaries, and headers from Homebrew into
# src-tauri/external/ffmpeg-macos/ and rewrites dylib load paths
# to use @loader_path/ so they work when bundled in the .app.
#
# Run this once after `brew install ffmpeg` on a macOS build machine.
# The resulting external/ffmpeg-macos/ directory is committed to the repo.
#
# Usage: ./scripts/bundle-macos-deps.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
DEST="$PROJECT_DIR/src-tauri/external/ffmpeg-macos"

FFMPEG_PREFIX="$(brew --prefix ffmpeg 2>/dev/null || echo "")"
if [ -z "$FFMPEG_PREFIX" ] || [ ! -d "$FFMPEG_PREFIX" ]; then
    echo "ERROR: FFmpeg not found via Homebrew. Install with: brew install ffmpeg"
    exit 1
fi

echo "FFmpeg found at: $FFMPEG_PREFIX"
echo "Bundling to: $DEST"

# Clean and create directory structure
rm -rf "$DEST"
mkdir -p "$DEST/lib" "$DEST/bin" "$DEST/include"

# ── Headers ──
echo "Copying headers..."
cp -R "$FFMPEG_PREFIX/include/"* "$DEST/include/"

# ── Binaries ──
echo "Copying binaries..."
cp "$FFMPEG_PREFIX/bin/ffmpeg" "$DEST/bin/"
cp "$FFMPEG_PREFIX/bin/ffprobe" "$DEST/bin/"

# ── FFmpeg dylibs (versioned only, not symlinks) ──
echo "Copying FFmpeg dylibs..."
FFMPEG_LIBS=(
    libavformat libavcodec libavutil libswscale
    libswresample libavfilter libavdevice
)

for lib in "${FFMPEG_LIBS[@]}"; do
    # Find the versioned dylib (e.g., libavformat.62.12.100.dylib)
    VERSIONED=$(ls "$FFMPEG_PREFIX/lib/${lib}".*.*.*.dylib 2>/dev/null | head -1)
    if [ -n "$VERSIONED" ]; then
        cp "$VERSIONED" "$DEST/lib/"
        BASENAME=$(basename "$VERSIONED")
        # Create the major-version symlink (e.g., libavformat.62.dylib)
        MAJOR_NAME=$(echo "$BASENAME" | sed -E 's/\.[0-9]+\.[0-9]+\.dylib/.dylib/')
        ln -sf "$BASENAME" "$DEST/lib/$MAJOR_NAME"
        # Create the unversioned symlink (e.g., libavformat.dylib)
        PLAIN_NAME="${lib}.dylib"
        ln -sf "$BASENAME" "$DEST/lib/$PLAIN_NAME"
        echo "  $BASENAME"
    else
        echo "  WARNING: $lib not found"
    fi
done

# ── Transitive Homebrew dependencies ──
echo "Copying transitive dependencies..."

# Collect all non-system dylib deps from our FFmpeg libs
DEPS=""
for lib_file in "$DEST/lib/"*.*.*.dylib; do
    lib_deps=$(otool -L "$lib_file" 2>/dev/null | grep "/opt/homebrew" | awk '{print $1}' | grep -v "ffmpeg" || true)
    DEPS="$DEPS $lib_deps"
done
# Also from the binaries
for bin_file in "$DEST/bin/"*; do
    bin_deps=$(otool -L "$bin_file" 2>/dev/null | grep "/opt/homebrew" | awk '{print $1}' | grep -v "ffmpeg" || true)
    DEPS="$DEPS $bin_deps"
done

# Deduplicate and copy
UNIQUE_DEPS=$(echo "$DEPS" | tr ' ' '\n' | sort -u | grep -v "^$" || true)
for dep in $UNIQUE_DEPS; do
    # Resolve symlinks to get the real file
    REAL_PATH=$(realpath "$dep" 2>/dev/null || echo "$dep")
    if [ -f "$REAL_PATH" ]; then
        BASENAME=$(basename "$REAL_PATH")
        if [ ! -f "$DEST/lib/$BASENAME" ]; then
            cp "$REAL_PATH" "$DEST/lib/$BASENAME"
            # Also create the symlink name if different
            DEP_BASENAME=$(basename "$dep")
            if [ "$DEP_BASENAME" != "$BASENAME" ] && [ ! -e "$DEST/lib/$DEP_BASENAME" ]; then
                ln -sf "$BASENAME" "$DEST/lib/$DEP_BASENAME"
            fi
            echo "  $BASENAME (from $dep)"
        fi
    else
        echo "  WARNING: $dep not found"
    fi
done

# ── Rewrite dylib load paths to @loader_path/ ──
echo ""
echo "Rewriting dylib load paths..."

rewrite_paths() {
    local file="$1"
    local use_rpath="$2"  # @loader_path for dylibs, @executable_path/../lib for binaries

    # Get all /opt/homebrew references
    local deps=$(otool -L "$file" 2>/dev/null | grep "/opt/homebrew" | awk '{print $1}')

    for dep in $deps; do
        local dep_basename=$(basename "$dep")
        # Find the actual file in our lib dir (might be a symlink target)
        local target_name=""
        if [ -f "$DEST/lib/$dep_basename" ] || [ -L "$DEST/lib/$dep_basename" ]; then
            target_name="$dep_basename"
        else
            # Try to find by prefix match (e.g., libx264.165.dylib for libx264.165)
            target_name=$(ls "$DEST/lib/" | grep "^${dep_basename%%.*}" | head -1 || true)
        fi

        if [ -n "$target_name" ]; then
            install_name_tool -change "$dep" "${use_rpath}/${target_name}" "$file" 2>/dev/null || true
        fi
    done

    # Also rewrite the dylib's own ID
    if [[ "$file" == *.dylib ]]; then
        local own_name=$(basename "$file")
        install_name_tool -id "@loader_path/$own_name" "$file" 2>/dev/null || true
    fi
}

# Rewrite all dylibs
for lib_file in "$DEST/lib/"*.dylib; do
    [ -L "$lib_file" ] && continue  # skip symlinks
    rewrite_paths "$lib_file" "@loader_path"
    echo "  Rewrote: $(basename "$lib_file")"
done

# Rewrite binaries to point to ../lib/
for bin_file in "$DEST/bin/"*; do
    [ ! -f "$bin_file" ] && continue
    rewrite_paths "$bin_file" "@executable_path/../lib"
    echo "  Rewrote: $(basename "$bin_file")"
done

# ── Verify ──
echo ""
echo "Verifying no remaining /opt/homebrew references..."
BAD_REFS=$(grep -rl "/opt/homebrew" "$DEST/lib/" "$DEST/bin/" 2>/dev/null | head -5 || true)
if [ -n "$BAD_REFS" ]; then
    echo "WARNING: Binary references to /opt/homebrew still found in:"
    # Check with otool instead of grep (binary files)
    for f in "$DEST/lib/"*.dylib "$DEST/bin/"*; do
        [ -L "$f" ] && continue
        refs=$(otool -L "$f" 2>/dev/null | grep "/opt/homebrew" || true)
        if [ -n "$refs" ]; then
            echo "  $(basename "$f"):"
            echo "$refs" | sed 's/^/    /'
        fi
    done
else
    echo "  All clean!"
fi

# ── Summary ──
echo ""
echo "=== Bundle Summary ==="
echo "Dylibs:  $(ls "$DEST/lib/"*.dylib 2>/dev/null | wc -l | tr -d ' ')"
echo "Symlinks: $(find "$DEST/lib/" -type l 2>/dev/null | wc -l | tr -d ' ')"
echo "Binaries: $(ls "$DEST/bin/" 2>/dev/null | wc -l | tr -d ' ')"
echo "Headers:  $(find "$DEST/include/" -name '*.h' 2>/dev/null | wc -l | tr -d ' ')"
echo "Total size: $(du -sh "$DEST" | awk '{print $1}')"
echo ""
echo "Done! Add src-tauri/external/ffmpeg-macos/ to your repo."
