#!/bin/bash
# Handle union:// URIs — open the path in the native file manager.
# Usage: open_union_link.sh "union:///lin/path/to/file"

URI="$1"

# Strip union:/// prefix
RAW="${URI#union:///}"

# Extract OS tag (default to 'lin' if absent)
SOURCE_OS="lin"
if [[ "$RAW" =~ ^(win|mac|lin)/ ]]; then
    SOURCE_OS="${BASH_REMATCH[1]}"
    RAW="${RAW#$SOURCE_OS/}"
fi

# URL-decode
PATH_DECODED=$(python3 -c "import urllib.parse, sys; print(urllib.parse.unquote(sys.argv[1]))" "$RAW" 2>/dev/null || echo "$RAW")

# Translate path if source OS differs
if [ "$SOURCE_OS" != "lin" ]; then
    SETTINGS_FILE="$HOME/.config/ufb/settings.json"
    if [ -f "$SETTINGS_FILE" ]; then
        # Use python3 to apply path mappings
        PATH_DECODED=$(python3 -c "
import json, sys
path = sys.argv[1]
source_os = sys.argv[2]
try:
    with open('$SETTINGS_FILE') as f:
        settings = json.load(f)
    for m in settings.get('pathMappings', []):
        src = m.get(source_os, '')
        dst = m.get('lin', '')
        if src and dst and path.startswith(src):
            path = dst + path[len(src):]
            break
except:
    pass
print(path)
" "$PATH_DECODED" "$SOURCE_OS" 2>/dev/null || echo "$PATH_DECODED")
    fi
fi

# Convert backslashes to forward slashes
PATH_DECODED="${PATH_DECODED//\\//}"

# Open in file manager
if [ -f "$PATH_DECODED" ]; then
    # File: open parent and select (if supported)
    xdg-open "$(dirname "$PATH_DECODED")"
elif [ -d "$PATH_DECODED" ]; then
    xdg-open "$PATH_DECODED"
else
    # Try parent directory
    PARENT="$(dirname "$PATH_DECODED")"
    if [ -d "$PARENT" ]; then
        xdg-open "$PARENT"
    fi
fi
