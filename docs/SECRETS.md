# Ephemeral Secret Handoff

Job Harness **never** stores passwords, portal logins, or API tokens in your career workspace, Git history, SQLite ledger, or chat transcript.

When an agent legitimately needs a short-lived credential (for example, a Workday portal email + password or a one-time verification code):

1. **Self-serve email OTPs first:** If a verification code is sent to a mailbox the agent already has read access to (for example via connected Gmail/IMAP), the agent **must fetch the code from email itself first** rather than interrupting the user.
2. **Escalate only when unreachable, with a bounded lease:** Only request human intervention when the credential is a password, SMS/authenticator code, or sent to an unlinked inbox. Requests carry a strict TTL/lease (default `300s`) — if unanswered within the wait window, the agent pauses only that single role and continues other work cleanly instead of stalling the whole batch.
3. **Inside an ADE with native secrets (e.g. T3 Code):** Use the host ADE's built-in `request_secret` card first.
4. **Inside Job Harness Desktop or standalone CLI (`Claude`, `Codex`, `Hermes`):** Use `jh secret` — an owner-only (`0700` dir / `0600` files) ephemeral vault in OS temp storage (`$TMPDIR/job-harness-secrets-<user>/`) with automatic self-destruction.

---

## How It Works

```text
Agent requests secret (metadata only: label, reason, fields, TTL)
   ↓
Desktop GUI pops a focused Secret Modal (or CLI: `jh secret fulfill <id>`)
   ↓  (password fields masked ••••••; never written to chat or metadata)
User submits → one `0600` file per field armed with TTL countdown (default 5m)
   ↓
Agent reads once (`jh secret read <id> <field>` or `$(cat <path>)`)
   ↓
Immediate self-destruct: file overwritten with 0x00 bytes + unlinked
```

### Security & Visibility Guarantees

- **Zero chat-history leakage:** Secret values never appear in prompts, tool arguments, Markdown notes, or `req-*.meta.json`.
- **Owner-only temp storage:** Stored exclusively in OS temp space with `0700` directory and `0600` file permissions — outside your workspace repo.
- **Single-use by default:** Reading a secret decrements `uses_remaining` and immediately zero-overwrites (`0x00`) and unlinks the payload file when exhausted.
- **Strict TTL self-destruct:** Default `300s` (5 minutes, hard-capped at `900s`). Expired secrets are automatically overwritten and deleted on the next tick or sweep even if never read.
- **Always visible & revocable:** While armed, the Desktop UI displays a live countdown badge (`🔒 <Label> · 4:42 · Destroy`) so you can detonate it with one click — and confirms with a toast once consumed and shredded.

---

## CLI Usage

```sh
# 1. Agent requests a portal login (returns request_id; never holds secrets)
jh secret request \
  --label "Workday Sign-In" \
  --reason "Sign in to submit Senior Backend Engineer application" \
  --url "https://company.wd3.myworkdayjobs.com" \
  --field "email:Email:text" \
  --field "password:Password:secret" \
  --ttl 300

# 2. Human fulfills privately in the Desktop Secret Modal (auto-opens)
#    OR in any terminal with hidden input (no shell history):
jh secret fulfill <request_id>

# 3. Agent consumes once (or fetches 0600 file paths for `$(cat <file>)`)
jh secret read <request_id> email
jh secret read <request_id> password   # auto-shreds on final field read!

# 4. Inspect active requests or immediately destroy
jh secret list
jh secret revoke <request_id>
```
