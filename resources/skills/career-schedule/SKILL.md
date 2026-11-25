---
name: career-schedule
description: Arrange authorised recruitment meetings, confirm timezone and calendar availability, book employer slots, accept invitations and update the candidate's pipeline.
---

# Schedule hiring conversations

Read mentioned sources through read_record (batch related reads); generic guides/skills may return origin=toolkit. For native file tools, use the returned ledger/toolkit root for each source. Avoid repeatedly loading broad views or unrelated context.

Resolve the ledger root with bootstrap, then read its AGENTS.md, [operations guide](../../guides/career-operations.md), exact vacancy, current thread and scheduling defaults. Start a schedule run scoped to the company/record. An options question authorises inspection, not sending or booking. For requested scheduling, complete the authorised reply/booking/calendar steps without asking again about routine choices.

Read the latest employer request and later messages; don't rebook a superseded reminder. Always inspect the candidate's live primary calendar first (existing events, configured working hours, prep/recovery buffers and timezone) before proposing or booking any slot. Follow the candidate's selected time over an earlier general preference. State date, time and timezone explicitly using configured preferences and employer evidence; resolve named-zone versus fixed-offset ambiguities. Retrieve the fallback booking link from saved answers; actual available slots control commitments.

Rank times with `suggest_meeting_times` before proposing or picking any slot. Classify the meeting as `screen` (recruiter or intro call), `interview` (hiring manager or team), `deep` (technical or final) or `assessment` (a block to do an online test or take-home against its deadline). Pass the live calendar's busy periods for the search range, tagging known interview events with their kind, plus the employer's offered times as `candidates` when a booking page lists them. Copy every time with its explicit offset or `Z`; convert the employer's named zone yourself only through the event's own offset. The workspace `scheduling` settings hold the candidate's preferred local windows, daily limits, buffers and notice. Windows only rank slots, so an out-of-window time can still be offered with its reason when nothing better exists. Hard rules (clashes with buffers, daily limits, meeting hours, notice and assessment deadline margins) are never overridden without the candidate's explicit say-so. Quote the returned `label` (local time with zone) in replies, and book an `assessment` block in the calendar so later bookings can't take it.

Scheduling is always human-confirmed for new time commitments:
- Calendars rarely capture every personal or offline commitment, so **never silently lock in a brand-new unapproved interview slot** on an employer's page just because a calendar gap looks empty.
- Inspect the employer's link or email and the live primary calendar (working hours + buffers), then draft a concise reply proposing 2–3 concrete conflict-free slots with short rationale, or surface the top 2–3 open employer link slots for one-tap human confirmation.
- Once the candidate picks/approves a slot (or an employer confirms a slot the candidate already offered), complete the booking confirmation, accept the calendar invite, and update the pipeline without extra round-trips.
- Hand off cleanly to the human with exact conflict or blocker details whenever all offered slots clash, a booking page requires login/CAPTCHA, the employer asks for special logistics (onsite travel, multi-part panel order, recording consent), or timing preferences are missing.

An availability reply is proposed or availability-sent, never booked. After booking verify the employer confirmation and exact time. Accept the existing calendar invitation; avoid creating a duplicate event. If only a proposal exists, a requested personal hold must be clearly tentative and have no employer guests. Honour saved recording/transcription preferences without inferring consent from the booking.

Send a routine confirmation or earlier-date request when authorised. Save exact sent wording/ID and before/after evidence. Log booking_confirmed, calendar_accepted and reply_sent as separate source-keyed events; set scheduled_start only with booked state and an explicit UTC offset. Update pipeline automatically through update_application. Finish with booked/proposed distinction, calendar proof and pending employer decisions. A passing date does not establish completion.

In focused MCP, load named specialised schemas with inspect_tools and run them as batch steps. Use query_batch for related reads and execute_batch for ordered local updates; pipe prior result fields with $ref. Read the version 2 data/error/next envelope. Page until next_offset is null when claiming exhaustive coverage. Use plan_adapter and record_adapter_result for requested external board sync, with actual live observations. See the [tool contract](../../guides/job-harness-contract.md) when recovering or composing batches.
