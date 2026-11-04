# Working with alarms

For support staff. This describes the dashboard's alarm pages; the rules
behind them are in `spec/15-dashboard.md` §5 and §10.2.

## What the three alarms watch

| Alarm | Watches | Goes critical when |
| --- | --- | --- |
| Incoming | The last message received for a flow | None has arrived for longer than the threshold for the time of day (day, evening, weekend, UK time) |
| Outgoing | The last message sent for a flow | The same, for sending |
| Failures | Messages that failed in a flow | The number in the rule's window reaches the threshold |

A rule with no data at all shows **Unknown**, not Healthy: a silent query and
a quiet flow are different things. If the alarm store itself cannot be read
the Alarms page says so in a banner; that is an outage to report, not "no
rules".

## Reading the Alarms page

Open **Alarms**. Rows are ordered Paused, Critical, Suppressed, Unknown,
Healthy.

- **Critical** — in alarm now. An e-mail has gone, if the rule has e-mail on
  (and, outside office hours, only if the rule allows out-of-hours e-mail).
- **Suppressed** — still in alarm, but inside the gap since the last e-mail,
  so no further e-mail is sent.
- **Paused** — someone asked for silence. The row says until when, why, and who.
  A *pause starts* note on any row means a pause is scheduled for it.

The line above the tables, *N paused / M scheduled*, links to the pauses page.

## Pausing alarms

Pause before planned maintenance, so the alarms do not e-mail for something
you caused on purpose. Open **Pauses**, then use **Pause alarms**.

1. **Applies to** — every alarm of every flow; every alarm of chosen flows
   (this includes alarm rules added to those flows later); or chosen alarm
   rules.
2. **Starts** — leave empty to start now, or enter a UK time up to a year
   ahead. A time in the spring-forward gap (01:00–02:00 on the last Sunday
   of March) does not exist and is refused; an ambiguous autumn time is the
   first one (BST).
3. **Ends** — after a duration (1 minute to 365 days), at a set UK time, or
   when cancelled. A pause that ends when cancelled stays until someone ends
   it, so use it with a reason you will recognise.
4. **Reason** (up to 200 characters) and **Requested by** (up to 100) are
   required. They are shown to everyone.

A paused rule is not evaluated and sends no e-mail. When the pause ends, or
you end it, evaluation resumes by itself. A cooldown that was running before
the pause is kept through it.

## Ending or cancelling a pause

On the pauses page, **End now** on an active pause or **Cancel** on a
scheduled one. An ended pause stays in *Recently ended* for seven days, then
is deleted after thirty.

## When pausing does not work

- **"Pause persistence is not configured."** The dashboard has no Cosmos
  account (`COSMOS_ENDPOINT`), so a pause could not be kept. It is refused
  rather than accepted and forgotten.
- **"Pause persistence is currently unavailable."** Cosmos could not be
  reached. Nothing was changed; try again.
- **A banner saying the pauses could not be read.** Alarms are being
  evaluated *without* pauses, so a paused alarm may still e-mail. Fix Cosmos
  access; this is deliberate — an outage must not silence the alarms.
- **"Unknown alarm rule"** — the rule was deleted since the page loaded.
  Reload.
