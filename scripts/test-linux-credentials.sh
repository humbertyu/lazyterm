#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != Linux ]]; then
  echo "This fixture requires Linux; it must not access the macOS Keychain." >&2
  exit 1
fi

for dependency in dbus-run-session gnome-keyring-daemon gdbus cargo timeout; do
  command -v "$dependency" >/dev/null
done

cargo test --locked --no-run

# Use a disposable home and session bus, never the user's credential collection.
fixture_root="$(mktemp -d)"
trap 'rm -rf "$fixture_root"' EXIT
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export HOME="$fixture_root/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_CACHE_HOME="$HOME/.cache"
export XDG_RUNTIME_DIR="$fixture_root/runtime"
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
unset DISPLAY WAYLAND_DISPLAY GNOME_KEYRING_CONTROL

LAZYTERM_SECRET_SERVICE_FIXTURE=unavailable \
  DBUS_SESSION_BUS_ADDRESS="unix:path=$fixture_root/missing-bus" \
  timeout 90s cargo test --locked \
  project_credentials::tests::linux_secret_service_unavailable -- \
  --exact --ignored --test-threads=1

export LAZYTERM_SECRET_SERVICE_FIXTURE=available
timeout 120s dbus-run-session -- bash -euo pipefail -c '
  printf "%s" "disposable-fixture-unlock" |
    gnome-keyring-daemon --unlock --components=secrets >/dev/null
  cargo test --locked project_credentials::tests::linux_secret_service_round_trip -- \
    --exact --ignored --test-threads=1
  cargo test --locked project_credentials::tests::linux_secret_service_locked -- \
    --exact --ignored --test-threads=1
'
