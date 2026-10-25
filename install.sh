#!/bin/sh
set -eu

umask 077

if [ "$(uname -s)" != Linux ]; then
    printf '%s\n' 'semmem installation requires Linux with a systemd user manager.' >&2
    exit 1
fi

if ! command -v systemctl >/dev/null 2>&1; then
    printf '%s\n' 'systemctl is unavailable. Install systemd and run this installer in your Linux user session.' >&2
    exit 1
fi

if ! systemctl --user show-environment >/dev/null; then
    printf '%s\n' 'The systemd user manager is unavailable. Start a systemd user session and rerun this installer.' >&2
    exit 1
fi

: "${HOME:?HOME must identify the installing user.}"
checkout=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
config_root=${XDG_CONFIG_HOME:-"$HOME/.config"}
data_root=${XDG_DATA_HOME:-"$HOME/.local/share"}
binary_root=$HOME/.local/bin
config_directory=$config_root/semmem
unit_directory=$config_root/systemd/user
data_directory=$data_root/semmem
environment_file=$config_directory/environment
binary=$binary_root/semmem

case "$HOME$config_root$data_root" in
    *'
'*)
        printf '%s\n' 'Installation paths cannot contain newlines in systemd unit files.' >&2
        exit 1
        ;;
esac

quote_unit() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g; s/%/%%/g'
}

quote_exec() {
    quote_unit "$1" | sed 's/\$/$$/g'
}

quote_path() {
    printf '%s' "$1" | sed 's/%/%%/g'
}

if [ -n "${SEMMEM_BINARY:-}" ]; then
    source_binary=$SEMMEM_BINARY
else
    cd "$checkout"
    cargo build --locked --release --bin semmem
    source_binary=${CARGO_TARGET_DIR:-"$checkout/target"}/release/semmem
fi

if [ ! -f "$source_binary" ] || [ ! -x "$source_binary" ]; then
    printf 'No executable semmem binary was found at %s.\n' "$source_binary" >&2
    exit 1
fi

mkdir -p "$binary_root" "$config_directory" "$unit_directory"
chmod 0700 "$config_directory"

if [ "$source_binary" != "$binary" ]; then
    install -m 0755 "$source_binary" "$binary"
fi

if [ ! -e "$environment_file" ] && [ ! -L "$environment_file" ]; then
    cat >"$environment_file" <<'CONFIG'
# Optional remote MCP settings. Values use systemd EnvironmentFile syntax.
# SEMMEM_ISSUER=https://identity.example.com
# SEMMEM_AUDIENCE=https://memory.example.com/mcp
# SEMMEM_PUBLIC_URL=https://memory.example.com
CONFIG
fi

chmod 0600 "$environment_file"

cat >"$unit_directory/semmem.service" <<UNIT
[Unit]
Description=semmem memory service
StartLimitIntervalSec=0

[Service]
Type=simple
UMask=0077
TimeoutStartSec=infinity
TimeoutStopSec=infinity
Environment="HOME=$(quote_unit "$HOME")"
Environment="XDG_CONFIG_HOME=$(quote_unit "$config_root")"
Environment="XDG_DATA_HOME=$(quote_unit "$data_root")"
EnvironmentFile=-$(quote_path "$environment_file")
ExecStart=/usr/bin/env "$(quote_exec "$binary")" --socket "$(quote_exec "$data_directory/service.sock")" run --data-dir "$(quote_exec "$data_directory")"
Restart=on-failure

[Install]
WantedBy=default.target
UNIT

chmod 0600 "$unit_directory/semmem.service"
systemctl --user daemon-reload
systemctl --user enable semmem.service
printf 'Installed %s and the semmem.service user unit.\n' "$binary"
printf 'Configuration: %s\n' "$environment_file"
printf '%s\n' 'Run semmem start to start the service. Existing memory data was preserved.'
