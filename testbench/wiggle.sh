#!/usr/bin/env bash
# Move the pointer around on the padlink output of the nested instance for
# N seconds (default 12): light, steady damage, like ordinary desktop use.
set -euo pipefail
env_file="${XDG_RUNTIME_DIR:?}/padlink-testbench/env"
[[ -f "$env_file" ]] || { echo "no nested instance running" >&2; exit 1; }
# shellcheck disable=SC1090
source "$env_file"
h=(hyprctl -i "$HYPRLAND_INSTANCE_SIGNATURE")
read -r x y < <("${h[@]}" -j monitors | tr -d '\n' | grep -o '"name": "padlink-[^}]*' | head -1 \
    | sed -E 's/.*"x": (-?[0-9]+),.*"y": (-?[0-9]+),.*/\1 \2/')
end=$((SECONDS + ${1:-12})); i=0
while (( SECONDS < end )); do
    i=$((i + 1))
    "${h[@]}" dispatch "hl.dsp.cursor.move({ x = $((x + 100 + (i * 13) % 900)), y = $((y + 100 + (i * 7) % 600)) })" >/dev/null
done
echo "moved the pointer $i times"
