#!/bin/bash
# build-ffmpeg-macos.sh
#
# Builds a minimal static FFmpeg from source for macOS arm64.
# Produces:
#   - Static libraries (.a) for linking into the Rust binary (thumbnail FFI)
#   - Static ffmpeg/ffprobe binaries for transcode (zero runtime deps)
#   - Headers for compilation
#
# Prerequisites: Xcode Command Line Tools, nasm/yasm
#   brew install nasm
#
# Usage: ./scripts/build-ffmpeg-macos.sh
#
# Output: src-tauri/external/ffmpeg-macos/ with bin/, lib/, include/

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BUILD_DIR="/tmp/ffmpeg-build"
PREFIX="$BUILD_DIR/install"
DEST="$PROJECT_DIR/src-tauri/external/ffmpeg-macos"
NPROC=$(sysctl -n hw.ncpu)

echo "Building FFmpeg from source (static, arm64)"
echo "Build dir: $BUILD_DIR"
echo "Install prefix: $PREFIX"
echo "Final dest: $DEST"
echo "Parallel jobs: $NPROC"
echo ""

mkdir -p "$BUILD_DIR"
cd "$BUILD_DIR"

# ── 1. Build x264 (H.264 encoder for transcode) ──
if [ ! -f "$PREFIX/lib/libx264.a" ]; then
    echo "=== Building x264 ==="
    if [ ! -d "x264" ]; then
        git clone --depth 1 https://code.videolan.org/videolan/x264.git
    fi
    cd x264
    ./configure \
        --prefix="$PREFIX" \
        --enable-static \
        --disable-shared \
        --disable-cli \
        --enable-pic \
        --extra-cflags="-arch arm64" \
        --extra-ldflags="-arch arm64"
    make -j"$NPROC"
    make install
    cd "$BUILD_DIR"
    echo "x264 done"
else
    echo "x264 already built, skipping"
fi

# ── 2. Build lame (MP3 encoder, needed by some containers) ──
if [ ! -f "$PREFIX/lib/libmp3lame.a" ]; then
    echo "=== Building lame ==="
    if [ ! -d "lame-3.100" ]; then
        curl -sL "https://sourceforge.net/projects/lame/files/lame/3.100/lame-3.100.tar.gz/download" -o lame.tar.gz
        tar xzf lame.tar.gz
    fi
    cd lame-3.100
    ./configure \
        --prefix="$PREFIX" \
        --enable-static \
        --disable-shared \
        --disable-frontend \
        --disable-decoder \
        --with-pic
    make -j"$NPROC"
    make install
    cd "$BUILD_DIR"
    echo "lame done"
else
    echo "lame already built, skipping"
fi

# ── 3. Build FFmpeg ──
if [ ! -f "$PREFIX/lib/libavformat.a" ]; then
    echo "=== Building FFmpeg ==="
    if [ ! -d "ffmpeg" ]; then
        git clone --depth 1 --branch release/7.1 https://github.com/FFmpeg/FFmpeg.git ffmpeg
    fi
    cd ffmpeg

    # Minimal build: decode everything, encode h264+aac only
    # Static libs + static binaries
    PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig" ./configure \
        --prefix="$PREFIX" \
        --enable-static \
        --disable-shared \
        --enable-pic \
        --enable-gpl \
        --enable-libx264 \
        --enable-libmp3lame \
        --enable-ffmpeg \
        --enable-ffprobe \
        --disable-ffplay \
        --disable-doc \
        --disable-htmlpages \
        --disable-manpages \
        --disable-podpages \
        --disable-txtpages \
        --disable-network \
        --disable-autodetect \
        --enable-audiotoolbox \
        --enable-videotoolbox \
        --enable-coreimage \
        --enable-avfoundation \
        --extra-cflags="-I$PREFIX/include" \
        --extra-ldflags="-L$PREFIX/lib" \
        --arch=arm64 \
        --cc="clang -arch arm64"

    make -j"$NPROC"
    make install
    cd "$BUILD_DIR"
    echo "FFmpeg done"
else
    echo "FFmpeg already built, skipping"
fi

# ── 4. Copy to project ──
echo ""
echo "=== Copying to $DEST ==="
rm -rf "$DEST"
mkdir -p "$DEST/lib" "$DEST/bin" "$DEST/include"

# Static libraries
cp "$PREFIX/lib/libavformat.a" "$DEST/lib/"
cp "$PREFIX/lib/libavcodec.a" "$DEST/lib/"
cp "$PREFIX/lib/libavutil.a" "$DEST/lib/"
cp "$PREFIX/lib/libswscale.a" "$DEST/lib/"
cp "$PREFIX/lib/libswresample.a" "$DEST/lib/"
cp "$PREFIX/lib/libx264.a" "$DEST/lib/"
cp "$PREFIX/lib/libmp3lame.a" "$DEST/lib/"

# Headers
cp -R "$PREFIX/include/"* "$DEST/include/"

# Static binaries
cp "$PREFIX/bin/ffmpeg" "$DEST/bin/"
cp "$PREFIX/bin/ffprobe" "$DEST/bin/"

# Sign the binaries (they're ours, built from source)
codesign --force --sign - "$DEST/bin/ffmpeg"
codesign --force --sign - "$DEST/bin/ffprobe"

# ── 5. Verify ──
echo ""
echo "=== Verification ==="
echo "ffmpeg deps:"
otool -L "$DEST/bin/ffmpeg" | grep -v "/usr/lib\|/System" || echo "  (none — fully static!)"
echo ""
echo "ffprobe deps:"
otool -L "$DEST/bin/ffprobe" | grep -v "/usr/lib\|/System" || echo "  (none — fully static!)"
echo ""

echo "Static libs:"
ls -lh "$DEST/lib/"*.a

echo ""
echo "Binary sizes:"
ls -lh "$DEST/bin/"*

echo ""
echo "Total size:"
du -sh "$DEST"

echo ""
echo "Done! FFmpeg built from source with static linking."
echo "No dylibs needed — everything links statically."
