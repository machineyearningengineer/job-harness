---
name: career-interview-prep
description: Prepare a concise company and screening brief for the candidate using the exact vacancy, earlier interviews, current company sources and truthful personal examples.
---

# Prepare a company conversation

For “Brief me for COMPANY/ROLE”, bootstrap `prepare_interview`, read `bootstrap.clock` and `settings.time_and_reminders` (`brief_lead_time_minutes`, `async_question_cutoff_minutes`), convert any relative interview time ("in 15 minutes") into an **absolute ISO deadline** upfront, read the private workspace's instructions, and resolve the exact canonical vacancy with `find_application`. Distinguish multiple roles at one company. If clarifying context would materially sharpen the round brief, ask asynchronously while continuing independent company/packet research; if unanswered by the cutoff (`no_answer`), stop researching early enough to deliver the finished brief before `brief_lead_time_minutes` ahead of the interview and flag open assumptions clearly. Start a bounded preparation run; this does not book or send anything.

Batch related reads through `query_batch`: `read_record` for the exact application and linked brief, `get_packet`/`preview_packet` for its frozen submission when available, `context_pack` for relevant project stories and original prior interview notes, and `daily_check` filtered by `company`/`record` for retained recruiting communications. Read linked communications evidence and its coverage; saved history is not a fresh inbox scan. Include useful supplied earlier prep conversations when available, without treating coaching as facts.

Use the actual submitted packet and answers when discussing what the employer has seen. Read the packet's frozen sources and their hashes rather than substituting the current CV or approved-answer file. If the submitted CV, answers, vacancy or earlier mail was not retained, state that gap; do not reconstruct an exact submission from today's baseline. For source passages, keep occurrence/recording dates, rough periods and pack gaps visible; read complete relevant originals through returned `read_refs` before making a claim.

Generic guides/skills returned by `read_record` may have `origin=toolkit`; installation does not require symlinks. Fetch specialized schemas with `inspect_tools` when needed. Use the [operations guide](../../guides/career-operations.md) and any existing private preparation template; avoid loading unrelated broad views or creating a competing company history.

Refresh material company facts from official sources and date them. Use a **two-layer brief** (`applications/briefs/template.md` when present) so company research is never repeated and each round gets the right lens:
- **Layer 1 — Company Brief (once per company, updated after every debrief):** product & business model (who buys, what problem it solves), **stage/archetype** (pre-seed, seed, Series A, Series B/scale-up, consultancy — what they buy vs. what they fear), what this exact role owns, how the candidate's past work maps in **product speak**, their **AI stance** (bounded delegation + verification + ownership), and unresolved terms.
- **Layer 2 — Round Brief (per round):** audience language split (Recruiter 80% product / 20% stack; HM/CTO 50/50; Technical 10/90; Design 30/70), a tight 60-second opener (Point → 2 proof points with numbers → 1 specific link to them), 2–3 **Point-first CARL** story cards from `context/profile/story-bank.md` with real scope/numbers/limits, relevant thinking hooks, and 2 high-signal questions to ask with hypothetical answer branches.
- **Post-Interview Debrief Loop:** after every round, record what landed, what missed, and new facts/constraints learned, then update Layer 1 before preparing the next round. Revise the linked canonical company brief instead of creating a competing one. No dedicated company-brief tool or new tracker is required.

Keep company self-report, recruiter account, user evidence, coaching and hypothetical answers distinct. Don't promote interview practice into CV facts. For roles with a system-design interview, begin with actors/decisions, requirements, invariants, durable data, flow, recovery and scale before tools.

Save the useful coaching and sources, link the brief in the vacancy record, log the update and finish with the brief plus material unanswered terms/process questions. Check local links and follow the ledger's commit rule for task-owned files.

In focused MCP, load named specialised schemas with inspect_tools and run them as batch steps. Use query_batch for related reads and execute_batch for ordered local updates; pipe prior result fields with $ref. Read the version 2 data/error/next envelope. Page until next_offset is null when claiming exhaustive coverage. Use plan_adapter and record_adapter_result for requested external board sync, with actual live observations. See the [tool contract](../../guides/job-harness-contract.md) when recovering or composing batches.
