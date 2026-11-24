#!/usr/bin/env python3
"""Seed a 100% synthetic Job Harness demo workspace (zero real personal data).

Usage:
    python3 scripts/seed_demo.py /tmp/job-harness-demo
    job-harness-desktop --root /tmp/job-harness-demo
"""
import json
import shutil
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

MIN_PDF = (
    b"%PDF-1.4\n"
    b"1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n"
    b"2 0 obj<</Type/Pages/Count 1/Kids[3 0 R]>>endobj\n"
    b"3 0 obj<</Type/Page/MediaBox[0 0 612 792]/Parent 2 0 R>>endobj\n"
    b"xref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n"
    b"0000000058 00000 n \n0000000115 00000 n \n"
    b"trailer<</Size 4/Root 1 0 R>>\nstartxref\n182\n%%EOF\n"
)


def main() -> None:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/job-harness-demo")
    if root.exists():
        shutil.rmtree(root)

    subprocess.run(
        ["jh", "setup", "--workspace", str(root)],
        stdin=subprocess.DEVNULL,
        check=True,
    )

    now = datetime.now(timezone.utc)
    today = now.strftime("%Y-%m-%d")
    in_3h = (now + timedelta(hours=3)).strftime("%Y-%m-%dT%H:30:00Z")
    in_2d = (now + timedelta(days=2)).strftime("%Y-%m-%dT14:00:00Z")
    yesterday = (now - timedelta(days=1)).strftime("%Y-%m-%d")
    week_ago = (now - timedelta(days=7)).strftime("%Y-%m-%d")
    two_weeks = (now - timedelta(days=16)).strftime("%Y-%m-%d")
    three_weeks = (now - timedelta(days=22)).strftime("%Y-%m-%d")

    cfg_path = root / "config/career-ledger.json"
    cfg = json.loads(cfg_path.read_text())
    cfg["timezone"] = "Europe/London"
    cfg["max_daily_submissions"] = 5
    cfg["daily_channel_limits"] = {"portal": 3, "direct_email": 2, "recruiter_outreach": 1}
    cfg["communication"]["sender_email"] = "maya@example.dev"
    cfg["time_and_reminders"]["pre_interview_reminder_minutes"] = 90
    cfg["time_and_reminders"]["applied_ghost_days"] = 14
    cfg["primary_cv"] = "output/tracks/backend/maya-lin-cv.pdf"
    cfg_path.write_text(json.dumps(cfg, indent=2) + "\n")

    for track in ("backend", "product", "ai"):
        p = root / f"output/tracks/{track}/maya-lin-cv.pdf"
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(MIN_PDF)

    (root / "context/profile").mkdir(parents=True, exist_ok=True)
    (root / "context/profile/application-decisions.md").write_text(
        "# Application Decisions (Maya Lin)\n\n"
        "- **Location:** London, UK (Hybrid 1–2d/wk or UK/EU remote)\n"
        "- **Salary floor:** £115,000 GBP\n"
        "- **Salary target:** £135,000 GBP\n"
        "- **Tracks:** Backend (`backend`), Applied AI (`ai`), Product (`product`)\n"
    )

    brief_dir = root / "applications/briefs"
    brief_dir.mkdir(parents=True, exist_ok=True)
    (brief_dir / "vektor-labs.md").write_text(
        "# Vektor Labs — Company & Round Brief\n\n"
        "## Layer 1 — Company Context\n\n"
        "- **Company & Role:** Vektor Labs · Staff Backend Engineer (Distributed Storage)\n"
        "- **What they build:** Low-latency vector + hybrid search engine for real-time observability (Series B, $42M raised, ~55 engineers).\n"
        "- **Salary & terms:** £130,000–£145,000 base + 0.15% equity · Hybrid London or Remote UK.\n"
        "- **Why it fits:** Direct match for Rust/Go storage pipelines, tail-latency debugging, and durable developer tooling.\n\n"
        "## Layer 2 — 10-Line Pre-Call Cheatsheet (HM Screen · Today)\n\n"
        "- **Interviewer:** Elena Rostova (VP Engineering) · 45 min\n"
        "- **Audience ratio:** 50% product impact / 50% distributed systems depth\n"
        "- **60-second opener:** Lead with cutting p99 latency 68% by moving hot reads onto a WAL-backed embedded index, then tie directly to Vektor's hybrid query planner.\n"
        "- **Top 2 proof points:**\n"
        "  1. **Zero-downtime shard migration:** Idempotent replay + shadow reads across 140M rows with zero customer incidents.\n"
        "  2. **Deterministic agent tool harness:** Typed CLI + SQLite WAL guardrails preventing duplicate side effects under retry.\n"
        "- **Questions to ask Elena:**\n"
        "  1. *Which part of the hybrid index path breaks first as tenants cross 10B vectors?*\n"
        "  2. *What does a genuinely strong first 90 days look like on the storage team?*\n"
    )

    apps = [
        ("2026-11-24-vektor-labs.md", "Vektor Labs — Staff Backend Engineer", {
            "stage": "screening", "status_detail": "HM screen booked today",
            "next_action": "Review 10-line pre-call cheatsheet 90m before Elena call",
            "checked": today, "schedule_state": "booked", "scheduled_start": in_3h,
            "track": "backend", "brief": "briefs/vektor-labs.md",
        }),
        ("2026-11-22-cinder-ai.md", "Cinder AI — Senior Product Engineer", {
            "stage": "technical", "status_detail": "Systems design round booked",
            "next_action": "Prep event-sourced replay diagram for Thursday panel",
            "checked": today, "schedule_state": "booked", "scheduled_start": in_2d, "track": "ai",
        }),
        ("2026-11-20-northstar-cloud.md", "Northstar Cloud — Principal Platform Engineer", {
            "stage": "final", "status_detail": "Final CTO & team panel completed",
            "next_action": "Draft concise thank-you note and share references",
            "checked": today, "track": "backend",
        }),
        ("2026-11-18-kestrel-pay.md", "Kestrel Pay — Lead Distributed Systems Engineer", {
            "stage": "offer", "status_detail": "Verbal offer received · reviewing equity",
            "next_action": "Compare total comp against Vektor & Northstar before counter",
            "checked": today, "track": "backend",
        }),
        ("2026-11-23-meridian-data.md", "Meridian Data — Senior Infrastructure Engineer", {
            "stage": "screening", "status_detail": "Recruiter proposed 3 intro slots",
            "next_action": "Confirm Tue 11:00 or Wed 15:00 slot in draft reply",
            "checked": today, "schedule_state": "proposed", "track": "backend",
        }),
        ("2026-11-21-prism-eval.md", "Prism Eval — Senior AI Platform Engineer", {
            "stage": "screening", "status_detail": "Intro call scheduled next week",
            "next_action": "Prepare 2 product stories from story bank", "checked": today, "track": "ai",
        }),
        ("2026-11-21-helios-robotics.md", "Helios Robotics — Full-Stack Telemetry Engineer", {
            "stage": "applied", "status_detail": "Submitted via Ashby · receipt logged",
            "next_action": "Wait for recruiter response", "checked": yesterday, "submitted_at": yesterday, "track": "product",
        }),
        ("2026-11-23-lattice-mesh.md", "Lattice Mesh — Senior Systems Engineer", {
            "stage": "applied", "status_detail": "Submitted via Greenhouse · Backend track",
            "next_action": "Await recruiter triage", "checked": yesterday, "submitted_at": yesterday, "track": "backend",
        }),
        ("2026-11-22-verge-compute.md", "Verge Compute — Staff Runtime Engineer", {
            "stage": "applied", "status_detail": "Submitted via Ashby · receipt verified",
            "next_action": "Follow up in 6 days if silent", "checked": yesterday, "submitted_at": yesterday, "track": "backend",
        }),
        ("2026-11-08-arbor-health.md", "Arbor Health — Senior Platform Engineer", {
            "stage": "applied", "status_detail": "Submitted 16d ago · quiet",
            "next_action": "Send polite check-in or archive", "checked": two_weeks, "submitted_at": two_weeks, "track": "backend",
        }),
        ("2026-11-24-trellis-security.md", "Trellis Security — Staff Detection Engineer", {
            "stage": "saved", "status_detail": "Scouted lead · strong Rust fit",
            "next_action": "Review JD and prepare Backend track packet", "checked": today, "track": "backend",
        }),
        ("2026-11-24-solstice-ai.md", "Solstice AI — Applied AI Systems Engineer", {
            "stage": "saved", "status_detail": "Scouted lead · matches AI track",
            "next_action": "Verify remote policy before outreach", "checked": today, "track": "ai",
        }),
        ("2026-11-24-stratos-db.md", "Stratos DB — Principal Storage Engineer", {
            "stage": "saved", "status_detail": "Scouted from HN Who's Hiring",
            "next_action": "Review posting and prepare tailored brief", "checked": today, "track": "backend",
        }),
        ("2026-11-10-cobalt-labs.md", "Cobalt Labs — Senior Backend Engineer", {
            "stage": "closed", "outcome": "rejected", "status_detail": "Rejected after HM screen",
            "next_action": "Closed", "checked": week_ago, "submitted_at": two_weeks, "track": "backend",
        }),
        ("2026-11-05-foundry-ops.md", "Foundry Ops — Staff Infrastructure Engineer", {
            "stage": "closed", "outcome": "rejected", "status_detail": "Rejected after technical screen",
            "next_action": "Closed", "checked": week_ago, "submitted_at": three_weeks, "track": "backend",
        }),
        ("2026-11-03-beacon-ai.md", "Beacon AI — Senior ML Systems Engineer", {
            "stage": "closed", "outcome": "rejected", "status_detail": "Automated ATS decline",
            "next_action": "Closed", "checked": two_weeks, "submitted_at": three_weeks, "track": "ai",
        }),
        ("2026-11-12-quartz-fin.md", "Quartz Finance — Staff Software Engineer", {
            "stage": "closed", "outcome": "withdrawn", "status_detail": "Withdrawn (5d onsite cadence)",
            "next_action": "Closed", "checked": week_ago, "submitted_at": two_weeks, "track": "backend",
        }),
        ("2026-11-01-harbor-labs.md", "Harbor Labs — Platform Engineer", {
            "stage": "closed", "status_detail": "Role closed by employer before screening",
            "next_action": "Archived", "checked": two_weeks, "submitted_at": three_weeks, "track": "backend",
        }),
    ]

    apps_dir = root / "applications"
    apps_dir.mkdir(parents=True, exist_ok=True)
    for fname, title, tracking in apps:
        md = f"# {title}\n\n<!-- tracking\n{json.dumps(tracking, indent=2)}\n-->\n"
        (apps_dir / fname).write_text(md)

    subprocess.run(["jh", "--root", str(root), "pipeline"], check=True)
    print(f"Seeded {len(apps)} synthetic demo roles at {root}")


if __name__ == "__main__":
    main()
