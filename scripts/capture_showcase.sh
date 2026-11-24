#!/usr/bin/env bash
# Reproducible, zero-personal-data showcase capture pipeline for Job Harness.
# Seeds a fresh synthetic 18-role workspace via scripts/seed_demo.py, captures
# every key desktop scene into docs/assets/, and renders a lightweight demo.gif.
#
# Usage:
#   ./scripts/capture_showcase.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="$REPO_ROOT/docs/assets"
DEMO_ROOT="${JOB_HARNESS_DEMO_ROOT:-/tmp/job-harness-demo}"
DEMO_UI_STATE="/tmp/job-harness-demo-ui-state.json"
DEMO_MCP="/tmp/job-harness-demo-mcp.json"
DEMO_AGENT="/tmp/job-harness-demo-agent.sh"
APP_BIN="${JOB_HARNESS_DESKTOP_BIN:-$REPO_ROOT/apps/desktop/target/release/job-harness-desktop}"

if [ ! -x "$APP_BIN" ]; then
  echo "Building release desktop binary..."
  cargo build --release --manifest-path "$REPO_ROOT/apps/desktop/Cargo.toml"
fi

python3 "$REPO_ROOT/scripts/seed_demo.py" "$DEMO_ROOT"
mkdir -p "$OUT_DIR"

cat <<'EOF' > "$DEMO_UI_STATE"
{
  "reduce_motion": false,
  "show_archive": false,
  "only_attention": false,
  "interviews_expanded": true,
  "collapsed_cols": [],
  "chat_open": true,
  "chat_width": 350,
  "drawer_width": 410,
  "agent_kind": "custom",
  "agent": { "model": null, "provider": null, "reasoning": null, "fast_mode": true },
  "last_seen_activity_at": null
}
EOF

cat <<'EOF' > "$DEMO_MCP"
{
  "mcpServers": {
    "career-ledger": { "command": "jh" },
    "gmail": { "command": "mcp-google" },
    "calendar": { "command": "mcp-google" },
    "web-search": { "command": "mcp-exa" }
  }
}
EOF

cat <<'EOF' > "$DEMO_AGENT"
#!/bin/sh
cat >/dev/null
printf '%s\n' '{"type":"tool_use","name":"terminal","tool_call_id":"t1","input":{"command":"jh status"}}'
sleep 0.2
printf '%s\n' '{"type":"tool_result","name":"terminal","tool_call_id":"t1","duration_ms":110,"is_error":false,"output":"{\"schema_version\":2,\"ok\":true,\"data\":{\"results\":[{\"tool\":\"bootstrap\",\"result\":{\"ok\":true,\"data\":{\"health\":{\"record_count\":18}}}}]}}"}'
printf '%s\n' '{"type":"tool_use","name":"read_file","tool_call_id":"t2","input":{"path":"applications/briefs/vektor-labs.md"}}'
sleep 0.2
printf '%s\n' '{"type":"tool_result","name":"read_file","tool_call_id":"t2","duration_ms":42,"is_error":false,"output":"Loaded Layer 1 company context + Layer 2 pre-call cheatsheet"}'
printf '%s\n' '{"type":"text","text":"### Today\u2019s priorities\n\n1. **Vektor Labs — HM Screen in 3h** (`applications/briefs/vektor-labs.md`)\n   - **Interviewer:** Elena Rostova (VP Engineering) · £130k–£145k · 50/50 product & storage depth.\n   - **Opener:** Lead with cutting p99 latency 68% via a WAL-backed embedded index, then bridge to Vektor\u2019s hybrid query planner.\n2. **Kestrel Pay — Verbal Offer Review** (`applications/2026-11-18-kestrel-pay.md`)\n   - Compare £138k + 0.12% equity against Vektor & Northstar before countering.\n3. **Meridian Data — Confirm Intro Slot** (`applications/2026-11-23-meridian-data.md`)\n   - Draft reply ready in Gmail for **Tue 11:00** or **Wed 15:00** (checked against live calendar buffers).\n\n*Permissions are in review-only mode: zero external emails or portal applications will send without your approval.*"}'
printf '%s\n' '{"type":"result","usage":{"total_tokens":1840}}'
EOF
chmod +x "$DEMO_AGENT"

capture_scene() {
  local scene="$1"
  local dest="$2"
  local wait_s="$3"
  JOB_HARNESS_UI_STATE_PATH="$DEMO_UI_STATE" \
  JOB_HARNESS_AGENT_CMD="$DEMO_AGENT" \
  JOB_HARNESS_AGENT_NAME="Agent" \
  JOB_HARNESS_MCP_CONFIG="$DEMO_MCP" \
  JOB_HARNESS_SHOWCASE_SCENE="$scene" \
  "$APP_BIN" --root "$DEMO_ROOT" >/tmp/job-harness-demo.log 2>&1 &
  local pid=$!
  sleep "$wait_s"
  local wid
  wid=$(swift -e '
import CoreGraphics
import Foundation
let pid = Int32(CommandLine.arguments[1])!
let list = CGWindowListCopyWindowInfo(.optionOnScreenOnly, kCGNullWindowID) as? [[String:Any]] ?? []
for w in list {
    if let p = w["kCGWindowOwnerPID"] as? Int32, p == pid, let num = w["kCGWindowNumber"] as? Int {
        print(num); break
    }
}
' "$pid")
  if [ -n "$wid" ]; then
    screencapture -x -l "$wid" "$dest"
  fi
  kill -TERM "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

capture_scene "brief"      "$OUT_DIR/showcase-brief.png"      2.6
capture_scene "hero"       "$OUT_DIR/showcase-hero.png"       2.6
capture_scene "flow"       "$OUT_DIR/showcase-flow.png"       2.6
capture_scene "flow-drill" "/tmp/showcase-flow-drill.png"     2.6
capture_scene "guardrails" "$OUT_DIR/showcase-guardrails.png" 1.8
capture_scene "command"    "$OUT_DIR/showcase-command.png"    1.8

# Compact <3MB animated walkthrough GIF for GitHub README
ffmpeg -y -loglevel error \
  -loop 1 -t 2.5 -i "$OUT_DIR/showcase-brief.png" \
  -loop 1 -t 2.2 -i "$OUT_DIR/showcase-hero.png" \
  -loop 1 -t 2.5 -i "$OUT_DIR/showcase-flow.png" \
  -loop 1 -t 2.4 -i "/tmp/showcase-flow-drill.png" \
  -loop 1 -t 2.4 -i "$OUT_DIR/showcase-guardrails.png" \
  -loop 1 -t 2.0 -i "$OUT_DIR/showcase-command.png" \
  -filter_complex "\
    [0:v]scale=960:622:force_original_aspect_ratio=decrease,pad=960:622:(ow-iw)/2:(oh-ih)/2:color=0x0b0b0e,fps=6,format=yuva420p,settb=AVTB[v0];\
    [1:v]scale=960:622:force_original_aspect_ratio=decrease,pad=960:622:(ow-iw)/2:(oh-ih)/2:color=0x0b0b0e,fps=6,format=yuva420p,settb=AVTB[v1];\
    [2:v]scale=960:622:force_original_aspect_ratio=decrease,pad=960:622:(ow-iw)/2:(oh-ih)/2:color=0x0b0b0e,fps=6,format=yuva420p,settb=AVTB[v2];\
    [3:v]scale=960:622:force_original_aspect_ratio=decrease,pad=960:622:(ow-iw)/2:(oh-ih)/2:color=0x0b0b0e,fps=6,format=yuva420p,settb=AVTB[v3];\
    [4:v]scale=960:622:force_original_aspect_ratio=decrease,pad=960:622:(ow-iw)/2:(oh-ih)/2:color=0x0b0b0e,fps=6,format=yuva420p,settb=AVTB[v4];\
    [5:v]scale=960:622:force_original_aspect_ratio=decrease,pad=960:622:(ow-iw)/2:(oh-ih)/2:color=0x0b0b0e,fps=6,format=yuva420p,settb=AVTB[v5];\
    [v0][v1]xfade=transition=fade:duration=0.25:offset=2.25[x01];\
    [x01][v2]xfade=transition=fade:duration=0.25:offset=4.20[x02];\
    [x02][v3]xfade=transition=fade:duration=0.25:offset=6.45[x03];\
    [x03][v4]xfade=transition=fade:duration=0.25:offset=8.60[x04];\
    [x04][v5]xfade=transition=fade:duration=0.25:offset=10.75,split[s0][s1];\
    [s0]palettegen=max_colors=96:stats_mode=diff[p];\
    [s1][p]paletteuse=dither=bayer:bayer_scale=3" \
  "$OUT_DIR/demo.gif"

ls -lh "$OUT_DIR"/showcase-*.png "$OUT_DIR/demo.gif"
