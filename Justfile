#!/usr/bin/env just --justfile
name := 'wlrix-tray'

rootdir := ''
prefix := '/usr'

base-dir := absolute_path(clean(rootdir / prefix))
bin-dir := base-dir / 'bin'

bin-src := 'target' / 'release' / name
bin-dst := bin-dir / name

default:
  @just --list

release:
  cargo build --release

lint:
  cargo clippy --all-targets -- -D warnings

test:
  cargo test

# Install the tray.
#
# One file. There is deliberately no `.desktop` entry: the Toolchest builds its menu by scanning
# them, and the tray is not something a person starts -- `wlrix-session` starts it, and a second
# copy started by hand would fail on the watcher name and exit. Nor is there a systemd unit: it is
# a Wayland client and dies with the compositor, which is exactly what should happen to it.
#
# Deliberately does not build: this is normally run as root, and building as root leaves a target
# directory nobody can write to afterwards.
#
#     just release && sudo just install
[doc("Install the tray (build first; run as root)")]
install:
  #!/usr/bin/env bash
  set -euo pipefail
  if [ ! -x '{{bin-src}}' ]; then
    echo "no release build -- run 'just release' first" >&2
    exit 1
  fi
  install -Dm0755 '{{bin-src}}' '{{bin-dst}}'
  echo "installed {{bin-dst}}"
  echo
  echo "wlrix-session starts this by name off PATH, after wlrix-desktop -- which is"
  echo "load-bearing: both are wlr-layer-shell bottom surfaces and the later one sorts"
  echo "above. Nothing else needs configuring."

[doc("Remove what install put down")]
uninstall:
  #!/usr/bin/env bash
  set -euo pipefail
  rm -f '{{bin-dst}}'
  echo "removed {{bin-dst}}"
