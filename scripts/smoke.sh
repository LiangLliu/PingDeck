#!/usr/bin/env bash
# smoke.sh: isolated-HOME smoke of the RELEASE build. Required when a change touches startup, the
# database, the mock server, trace, IPC, secrets or the gateway. UI steps are not scripted: docs/product/smoke-checklist.md.
#
#   scripts/smoke.sh [path/to/pingdeck]        (default: $PINGDECK_BIN, then target/release/pingdeck)
#
# A relative binary path resolves against the repository root (the script cds there first).
#
# Never touches the real data directory: HOME (and XDG_*) point into a fresh `mktemp -d` and the script refuses to run
# when another `pingdeck` process exists (it would also be the target of the quit step). Steps:
#   1 launch once to create the database, wait for the schema, quit gracefully
#   2 seed (sqlite3): a workspace row and `ai` settings with gateway.enabled_on_start=true on a free port
#   3 launch from the seeded data directory: process stays alive, GET /health answers 200 (gateway auto-start),
#     data-directory modes (directories 0700, files 0600), graceful quit exits 0 and frees the port
#   4 relaunch: the seeded workspace reads back through a read-only SQLite query, graceful quit exits 0
#   5 macOS: pure-Rust GPUI runtime creates zero WebKit website data (neither real nor isolated)
# The expected schema version comes from `cargo xtask schema-fingerprint` (the binary must be built from these sources).
# Graceful quit = NSRunningApplication.terminate on macOS (what Cmd+Q does; a plain SIGTERM exits 143, the app installs no
# signal handler); on Linux `xdotool windowclose <window>` (or `wmctrl -ic <window>` fallback; needs xdotool/wmctrl and a
# display; xvfb-run is used when there is none). When no graceful method exists the step is reported SKIPPED and the app is
# stopped with SIGTERM.
#
# Exit codes: 0 every step passed (skipped steps are listed), 1 a step failed or the binary/tools are absent.

# No `set -e`: every step ends in an explicit `|| die`, and `wait "$PID"; QUIT_CODE=$?` must survive a non-zero exit
# so it can be reported as "exit code N, expected 0".
set -u
cd "$(dirname "$0")/.." || exit 1
ROOT=$PWD
OS=$(uname -s)

DB_TIMEOUT_S=40     # database file and schema version after the first launch
HEALTH_TIMEOUT_S=30 # gateway auto-start after the seeded launch
WINDOW_TIMEOUT_S=20 # X11 window mapping before graceful window close on Linux
QUIT_TIMEOUT_S=20   # process exit after a graceful quit
SETTLE_S=3          # a launch that survives this long counts as started
SCHEMA_SETTLE_S=1   # let the first launch finish its startup writes after the schema appears
POLL_S=0.25
# A stored `ai` section version the app accepts; see SETTINGS_VERSION in crates/pingdeck-model/src/settings/mod.rs.
AI_SETTINGS_VERSION=2

BIN=${1:-${PINGDECK_BIN:-$ROOT/target/release/pingdeck}}
STEPS_SKIPPED=()
H=""
PID=""

die() {
  echo "FAIL: $*" >&2
  # The isolated HOME is deleted on exit; keep both stdout/stderr and pingdeck.log visible in CI.
  [ -n "$H" ] && [ -s "$H/app.log" ] && { echo "--- last 40 lines of $H/app.log" >&2; tail -n 40 "$H/app.log" >&2; }
  [ -n "${DATA:-}" ] && [ -s "$DATA/logs/pingdeck.log" ] && {
    echo "--- last 40 lines of $DATA/logs/pingdeck.log" >&2
    tail -n 40 "$DATA/logs/pingdeck.log" >&2
  }
  exit 1
}
ok() { echo "ok:   $*"; }
skip() { # skip <step>: list it once in the final SKIPPED line
  local s # the `+` expansion keeps bash 3.2 (macOS /bin/bash) quiet about an empty array under `set -u`
  for s in ${STEPS_SKIPPED[@]+"${STEPS_SKIPPED[@]}"}; do [ "$s" = "$1" ] && return; done
  STEPS_SKIPPED+=("$1")
}
indent_lines() { printf '\n  %s' "${1//$'\n'/$'\n  '}"; } # one indented line per path, spaces kept
cleanup() {
  [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null && kill -KILL "$PID" 2>/dev/null
  # Optional CI diagnostic copy: preserve app.log and pingdeck.log before the isolated HOME is removed.
  if [ -n "${PINGDECK_SMOKE_LOG:-}" ] && [ -n "$H" ]; then
    mkdir -p "$(dirname "$PINGDECK_SMOKE_LOG")" 2>/dev/null || true
    {
      if [ -s "$H/app.log" ]; then
        echo "=== stdout/stderr ($H/app.log) ==="
        cat "$H/app.log"
      fi
      if [ -n "${DATA:-}" ] && [ -s "$DATA/logs/pingdeck.log" ]; then
        echo "=== diagnostic log ($DATA/logs/pingdeck.log) ==="
        cat "$DATA/logs/pingdeck.log"
      fi
    } >"$PINGDECK_SMOKE_LOG" 2>/dev/null || true
    [ -s "$PINGDECK_SMOKE_LOG" ] || rm -f "$PINGDECK_SMOKE_LOG" 2>/dev/null || true
  fi
  local tmp=${TMPDIR:-/tmp}
  case "$H" in "${tmp%/}"/*|/tmp/*|/var/folders/*) rm -rf "$H" ;; esac
}
trap cleanup EXIT

[ -x "$BIN" ] || die "binary not found: $BIN (build it first: cargo build --release -p pingdeck-gpui --locked; or pass the path)"
command -v sqlite3 >/dev/null || die "sqlite3 is required to seed and read back the database"
command -v curl >/dev/null || die "curl is required"
pgrep -x pingdeck >/dev/null && die "another pingdeck process is running; quit it first (this script would not touch it)"
if [ "$OS" = Linux ] && [ -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
  command -v xvfb-run >/dev/null && exec xvfb-run -a "$ROOT/scripts/smoke.sh" "$@"
  die "no display: set DISPLAY or install xvfb-run"
fi
# Before launching anything: compute expected schema fingerprint via pure-Rust xtask (no Node required).
want=$(cargo run -q -p xtask --locked -- schema-fingerprint) || die "cannot compute the schema fingerprint"

H=$(mktemp -d) || die "mktemp failed"
case "$OS" in
  Darwin) DATA="$H/Library/Application Support/com.pingdeck.desktop" ;;
  Linux) DATA="$H/.local/share/com.pingdeck.desktop" ;;
  *) die "unsupported OS $OS" ;;
esac
DB="$DATA/pingdeck.sqlite3"
REAL_WEBKIT="$HOME/Library/WebKit/pingdeck" # must stay untouched for the whole run
MARK=$H/.smoke-start
: >"$MARK"

launch() {
  # CFFIXED_USER_HOME as well as HOME: WebKit resolves its data directory through CoreFoundation and ignores HOME alone.
  # PINGDECK_DATA_DIR is dropped so the release build uses its default directory under $H; the client-tool config
  # variables are dropped so no client config outside $H is visible to the app (docs/development.md §4).
  env -u PINGDECK_DATA_DIR -u CLAUDE_CONFIG_DIR -u CODEX_HOME -u GEMINI_CLI_HOME \
    HOME="$H" CFFIXED_USER_HOME="$H" XDG_DATA_HOME="$H/.local/share" XDG_CONFIG_HOME="$H/.config" XDG_CACHE_HOME="$H/.cache" \
    "$BIN" >>"$H/app.log" 2>&1 &
  PID=$!
}
until_true() { # until_true <seconds> <command...>
  local end=$((SECONDS + $1)); shift
  while ! "$@" 2>/dev/null; do [ $SECONDS -ge $end ] && return 1; sleep "$POLL_S"; done
}
alive() { kill -0 "$PID" 2>/dev/null; }
gone() { ! alive; }
quit_app() { # graceful quit; sets QUIT_CODE (empty when skipped) and QUIT_ERR (why it failed, when known)
  QUIT_CODE=""
  QUIT_ERR=""
  case "$OS" in
    Darwin) osascript -l JavaScript -e "ObjC.import('AppKit'); \$.NSRunningApplication.runningApplicationWithProcessIdentifier($PID).terminate" >>"$H/app.log" 2>&1 ;;
    Linux)
      local closer=""
      if command -v xdotool >/dev/null; then
        closer="xdotool"
      elif command -v wmctrl >/dev/null; then
        closer="wmctrl"
      else
        skip "graceful quit (xdotool/wmctrl missing)"; kill -TERM "$PID"; wait "$PID" 2>/dev/null; return 0
      fi
      local w="" end=$((SECONDS + WINDOW_TIMEOUT_S))
      while alive; do
        if [ "$closer" = "xdotool" ]; then
          w=$(xdotool search --onlyvisible --pid "$PID" 2>/dev/null | head -n 1 || true)
          if [ -z "$w" ] && command -v xprop >/dev/null; then
            local cand cpid
            for cand in $(xprop -root _NET_CLIENT_LIST 2>/dev/null | sed -n 's/.*# //p' | tr ',' ' '); do
              cpid=$(xprop -id "$cand" _NET_WM_PID 2>/dev/null | awk '/= /{print $NF; exit}')
              if [ "$cpid" = "$PID" ]; then w="$cand"; break; fi
            done
          fi
        else
          w=$(wmctrl -lp 2>/dev/null | awk -v p="$PID" '$3 == p { print $1; exit }')
        fi
        [ -n "$w" ] && break
        [ "$SECONDS" -ge "$end" ] && break
        sleep "$POLL_S"
      done
      if [ -z "$w" ]; then
        {
          echo "[smoke] X11 window discovery failed (tool=$closer, wanted pid $PID):"
          command -v xdotool >/dev/null && { echo "--- xdotool search --pid $PID ---"; xdotool search --pid "$PID" 2>&1 || true; }
          command -v xprop >/dev/null && { echo "--- xprop -root _NET_CLIENT_LIST ---"; xprop -root _NET_CLIENT_LIST 2>&1 || true; }
          command -v xwininfo >/dev/null && { echo "--- xwininfo -root -tree ---"; xwininfo -root -tree 2>&1 || true; }
        } >>"$H/app.log"
        QUIT_ERR="no window for pid $PID to close ($closer)"; kill -KILL "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; return 1
      fi
      if [ "$closer" = "xdotool" ]; then
        xdotool windowclose "$w"
      else
        wmctrl -ic "$w"
      fi ;;
  esac
  until_true "$QUIT_TIMEOUT_S" gone || { kill -KILL "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; return 1; }
  wait "$PID"; QUIT_CODE=$?
}
quit_ok() { # quit_ok <step>
  quit_app || die "$1: ${QUIT_ERR:-the app did not exit within $QUIT_TIMEOUT_S s of a graceful quit}"
  [ -n "$QUIT_CODE" ] || { ok "$1: stopped with SIGTERM (graceful quit skipped)"; return; }
  [ "$QUIT_CODE" = 0 ] || die "$1: exit code $QUIT_CODE, expected 0"
  ok "$1: exited cleanly"
}

# 1 ── bootstrap: create the database
launch
until_true "$DB_TIMEOUT_S" test -f "$DB" || die "no database created in $DATA within $DB_TIMEOUT_S s (app log: $H/app.log)"
schema() { sqlite3 -readonly "$DB" 'PRAGMA user_version' 2>/dev/null; }
schema_ready() { [ "$(schema)" = "$want" ]; }
until_true "$DB_TIMEOUT_S" schema_ready || die "database schema is '$(schema)', the sources expect $want: the binary is older than the sources, rebuild it"
sleep "$SCHEMA_SETTLE_S"
alive || die "app exited during the first launch (see $H/app.log)"
quit_ok "first launch"

# 2 ── seed
PORT=$(cargo run -q -p xtask --locked -- free-port) || die "no free port"
sqlite3 "$DB" "
INSERT OR REPLACE INTO app_settings (section, version, value_json, updated_at_ms)
  VALUES ('ai', $AI_SETTINGS_VERSION, '{\"gateway\":{\"enabled_on_start\":true,\"port\":$PORT},\"agent\":{\"global_instructions\":\"Smoke: answer briefly.\",\"default_permission_mode\":\"ask\"}}', strftime('%s','now') * 1000);
INSERT INTO workspaces (id, name) VALUES ('ws-smoke-seed', 'Smoke seeded workspace');
" || die "seeding failed"
ok "seeded workspace, gateway auto-start on port $PORT and the agent's global instructions and default permission mode"

# 3 ── seeded launch
launch
sleep "$SETTLE_S"
alive || die "app exited within $SETTLE_S s of the seeded launch (see $H/app.log)"
ok "process alive after $SETTLE_S s"
until_true "$HEALTH_TIMEOUT_S" curl -fsS --max-time 2 -o /dev/null "http://127.0.0.1:$PORT/health" || die "GET /health on port $PORT never answered 200: the gateway did not auto-start (app log: $H/app.log)"
ok "GET /health 200 on 127.0.0.1:$PORT"
bad=$(find "$DATA" -type d ! -perm 0700 -print -o -type f ! -perm 0600 -print)
[ -z "$bad" ] || die "data directory modes (want dirs 0700, files 0600):$(indent_lines "$bad")"
ok "data directory modes (dirs 0700, files 0600)"
quit_ok "seeded launch"
curl -fsS --max-time 2 -o /dev/null "http://127.0.0.1:$PORT/health" 2>/dev/null && die "gateway still answers on port $PORT after quit"
ok "gateway port released after quit"

# 4 ── relaunch and read back
launch
sleep "$SETTLE_S"
alive || die "app exited within $SETTLE_S s of the relaunch (see $H/app.log)"
name=$(sqlite3 -readonly "$DB" "SELECT name FROM workspaces WHERE id = 'ws-smoke-seed'")
[ "$name" = "Smoke seeded workspace" ] || die "seeded workspace not read back (got '${name}')"
ok "seeded workspace read back after relaunch"
agent=$(sqlite3 -readonly "$DB" "SELECT json_extract(value_json, '\$.agent.global_instructions') || '|' || json_extract(value_json, '\$.agent.default_permission_mode') FROM app_settings WHERE section = 'ai'")
[ "$agent" = "Smoke: answer briefly.|ask" ] || die "agent settings not kept across launches (got '${agent}')"
ok "agent global instructions and default permission mode kept across launches"
quit_ok "relaunch"

# 5 ── Pure-Rust GPUI creates zero WebKit website data (neither real nor isolated HOME)
if [ "$OS" = Darwin ]; then
  touched=""
  [ ! -d "$REAL_WEBKIT" ] || touched=$(find "$REAL_WEBKIT" -newer "$MARK" -print 2>/dev/null)
  [ -z "$touched" ] || die "the app wrote into the real WebKit data directory $REAL_WEBKIT:$(indent_lines "$touched")"
  [ ! -d "$H/Library/WebKit/pingdeck" ] || die "unexpected WebKit website data created under $H/Library/WebKit/pingdeck"
  ok "zero WebKit data created (pure-Rust GPUI shell verified; real $REAL_WEBKIT untouched)"
fi

[ ${#STEPS_SKIPPED[@]} -eq 0 ] || echo "SKIPPED: ${STEPS_SKIPPED[*]}"
echo "smoke ok ($BIN)"
