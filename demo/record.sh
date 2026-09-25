#!/usr/bin/env bash
# Record assets/demo.gif and assets/screenshot.png with VHS against an
# isolated Herdr (own config dir + session), using demo/prs.json fixture data.
# Your normal Herdr setup is not touched.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
command -v vhs >/dev/null || { echo "install vhs: brew install vhs" >&2; exit 1; }

# Short path: Herdr's unix socket lives under the config dir and has a length limit.
tmp="$(mktemp -d /tmp/hdemo.XXXX)"
herdr_bin="$(command -v herdr)"
# Throwaway HOME: Herdr reads config from ~/.config/herdr, and the pane shell
# must not load your dotfiles (prompt, email, paths) into the recording.
export HOME="$tmp/home" XDG_CONFIG_HOME="$tmp/home/.config" XDG_STATE_HOME="$tmp/home/.local/state"
export SHELL=/bin/bash PS1='$ ' AGENT_PRS_ROOT="$root" PATH="$(dirname "$herdr_bin"):$PATH"
# Drop every HERDR_* variable: when run from inside Herdr, HERDR_SOCKET_PATH,
# HERDR_SESSION and HERDR_CONFIG_PATH would point the demo at your real Herdr.
for v in $(env | sed -n 's/^\(HERDR_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$v"; done
# A fake repo so Herdr's header reads "~/acme/api · feat/rate-limit".
mkdir -p "$HOME/acme/api"
# Pane shells are login shells; macOS /etc/bashrc would put host and user in PS1.
printf '%s\n' "PS1='$ '" 'export BASH_SILENCE_DEPRECATION_WARNING=1' > "$HOME/.bash_profile"
cp "$HOME/.bash_profile" "$HOME/.bashrc"
git -C "$HOME/acme/api" init -q -b feat/rate-limit
session=agentprs-demo
cleanup() {
  herdr --session "$session" server stop >/dev/null 2>&1 || true
  rm -rf "$tmp"
}
trap cleanup EXIT

mkdir -p "$XDG_CONFIG_HOME/herdr"
cp demo/herdr-config.toml "$XDG_CONFIG_HOME/herdr/config.toml"
herdr --session "$session" plugin link "$root" >/dev/null
cfg="$(herdr --session "$session" plugin config-dir sebassdc.agent-prs)"
mkdir -p "$cfg"
sed "s#__ROOT__#$root#" demo/plugin-config.toml > "$cfg/config.toml"

tape="$root/${1:-demo/demo.tape}"
cd "$HOME/acme/api"
vhs "$tape"
# The tape writes relative to this cwd; bring outputs back into the repo.
for d in assets demo/.debug; do
  if [ -d "$HOME/acme/api/$d" ]; then
    mkdir -p "$root/$d"
    cp -R "$HOME/acme/api/$d/." "$root/$d/"
  fi
done
