# Design, Schema & Writing Principles

These rules govern both the **native GPUI desktop workbench** (`apps/desktop`) and **every AI agent** operating on a Job Harness workspace.

---

## 1. Visual & Interaction Design (Desktop UI)

- **Standardized tight corner radius scale (`theme.rs`):**
  - `r_xs()` = `3.5px` (micro badges, keycaps, tiny icon tiles)
  - `r_sm()` = `5.0px` (buttons, rows, toggles, compact pills)
  - `r_md()` = `8.0px` (inputs, search bar, cards)
  - `r_lg()` = `11.0px` (popovers, modals, command palette)
  - Avoid generic 6/8/12/16px defaults; keep corner geometry sculpted and uniform.
- **Borderless inputs until focused:** Text inputs (`Filter roles…`, Chat composer, Quick-Ask, Secret fields) blend cleanly into their surface with transparent borders at rest and reveal `theme::hairline_strong()` only when focused.
- **No middle-dots (`·`) between UI words:** Use spacing, parentheses, slashes, or distinct visual hierarchy (badges, status dots, typography) instead of scattering `·` separators between labels.
- **Show, don't tell — `(i)` info buttons only for invisible side-effects:** Reserve `(i)` disclosures strictly for settings with external or hidden consequences (automation permissions, submission caps). Everywhere else, make the metric or control self-explanatory and directly interactive (e.g. tapping `10 upcoming →` focuses upcoming roles on the board).
- **Active-agent-scoped connector visibility:** In Settings, display only the MCP servers reachable by the currently selected agent runtime (`Shared` + `Only in <Agent>`). Never clutter the view with dimmed MCPs belonging to other runtimes.
- **Zero-shift expandable Kanban columns:** Expandable groups (`Interviews ›` → `‹ Technical / Final / Offer`, `Archive ›` → `‹ Rejected / Withdrawn / Other closed`) keep the primary column anchored at the exact same pixel and glide child columns in/out cleanly.
- **Popover anchoring:** Every popover opens directly beside its trigger button with a clean `10–12px` air gap and balanced width. Never stack popovers directly on top of their button borders.
- **Stateless chat default + read-only host session resume:** Launching the app or starting a new chat begins fresh to prevent context rot. Previous chats are read-only pointers into the host runtime's own store (zero second Job Harness chat DB).

---

## 2. Scannable Note & Brief Writing Contract (Agents)

Agents write for **both fast human scanning and low-token retrieval**:

- **Role `status_detail` (Summary):** Max **2 lines (≤ 160 characters)**. Format: `<Key fit or outcome> — <comp / location / key date>`. Never write multi-paragraph summaries in `status_detail`.
- **Role `next_action` (Next step):** One short action phrase (≤ 60 characters), ideally starting with a canonical action verb:
  - `review_fit` — saved lead awaiting go/no-go decision
  - `prepare_packet` — approved lead, ready to tailor CV & answers
  - `submit_application` — packet frozen, awaiting submit or human approval
  - `wait_employer` — applied/interviewed, waiting on employer
  - `follow_up` — quiet past threshold, draft follow-up
  - `reply_recruiter` — inbound recruiter message needs response
  - `book_interview` — pick or confirm interview slot
  - `prep_interview` — build or refresh 2-layer brief
  - `check_outcome` — `outcome_unknown`, verify live receipt/portal
  - `none` — closed
- **4 Canonical Semantic Buckets** for role notes and 2-layer briefs (avoid inventing 8–10 ad-hoc headings):
  1. `## Summary & fit` — 3–5 crisp key-value bullets (Comp, Location, CV track, Fit/risk)
  2. `## Prep & stories` — 10-line pre-call cheatsheet + 2–3 CARL story routes (1 line each)
  3. `## Questions & risks` — up to 3 likely interviewer questions + 2–3 sharp questions to ask
  4. `## Timeline` — reverse-chronological dated single-line events (`YYYY-MM-DD — <event>`)
- **Prose rule:** Zero walls of text longer than 3 lines. Lead every bullet with the decision, metric, or concrete fact — never filler.

---

## 3. Identity, Email Discrepancy & Security Boundaries

- **Two explicit emails (`config/career-ledger.json`):**
  - `application_email`: printed on CVs and entered in ATS portals (falls back to `baseline.json` contact email).
  - `sender_email` (Reply email): used to send/reply to recruiter threads (falls back to `application_email`).
  - Before sending mail, verify that `sender_email` matches the connected inbox or a verified Send-As alias; if there is any mismatch, **draft and flag the discrepancy** rather than silently sending from an unexpected address. When checking receipts or rejections, search mail addressed to **both** emails.
- **Avoid list (`discovery.avoid_list`):** Never apply to or pitch companies, sectors, or work patterns listed in the candidate's avoid list.
- **Secrets & Browser Sessions:** See [SECRETS.md](SECRETS.md) and [TOOLING.md](TOOLING.md) — never store credentials in workspace/chat/Git, never scrape personal browser cookies, and never bypass CAPTCHAs.
