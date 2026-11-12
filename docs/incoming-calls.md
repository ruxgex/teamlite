# Incoming calls: options

Status: **undecided**. Outgoing calls and meetings already work through the call window
(`teamlite-call`, Teams on the web in embedded Chromium). Teamlite can't ring for incoming calls
yet, because it hears about changes by polling every 2–5 seconds and has no push channel.

There are two ways to fix this. They solve different problems, so they can be combined.

---

## Option A: keep the call window running, hidden ("Receive calls")

Start `teamlite-call` hidden when Teamlite starts (or at login), on the Teams home page. When a
call rings, raise the window so the user can answer in Teams' own UI.

### How
- A setting, "Receive calls" (off by default). When it's on, Teamlite starts
  `teamlite-call --hidden --url=https://teams.microsoft.com/v2/`.
- In `teamlite-call`, add `--hidden` (window created but not shown, or minimized) and detect a
  ringing call by:
  - the page title changing (Teams sets "… is calling" / "Incoming call"), via
    `DisplayHandler::on_title_change`; or
  - a script injected on load that watches the page for the incoming-call panel and signals
    the call window (title change, or a CEF message to the browser process).
- On ring: show and focus the window, and optionally send a desktop notification.
- Teamlite's existing Call/Video/Open buttons keep working: they reach the running instance
  through the single-instance handoff.

### Pros
- Answering, declining, call waiting, transfers and calls from meetings all work, because Teams
  on the web is already registered as a calling device.
- It's the only reliable way to answer in time. A call rings for about 30 seconds; starting
  the call window cold and loading Teams takes about 5–15 seconds, and a freshly loaded client
  probably won't pick up a call that's already ringing (not verified).
- Little code: a setting, a hidden start, and ring detection.

### Cons
- Memory: Teams on the web uses roughly 500 MB–1 GB of RAM plus some background CPU while it
  runs. That's close to running the Teams PWA, which Teamlite is meant to avoid. This is why
  it's opt-in.
- Ring detection depends on Teams' page title or page structure, which Microsoft can change
  without notice.
- It doesn't help the chat side at all (still polling).

### Effort
About 1–2 days.

---

## Option B: implement Trouter (Teams' push websocket)

Trouter is the websocket that Teams on the web uses to receive push events. Teamlite would
connect to it directly, with no browser.

### How
- Register a Trouter endpoint with the Skype token, open the websocket, keep it alive
  (pings, reconnect, token refresh) for each organisation the user belongs to.
- Route the events it receives:
  - new or edited messages, reactions, read receipts → replace the 2–5 s polling;
  - typing → show "X is typing…";
  - presence changes → live presence dots;
  - incoming call → a native "X is calling" notification.
- It's undocumented: reverse-engineer it from Teams on the web's traffic (the call window's
  DevTools makes this easier).

### Pros
- Light: one websocket per organisation instead of a whole browser.
- Makes the chat side instant and lets Teamlite show when others are typing, which the README
  currently lists as not supported.
- Gives an "X is calling" notification even when no call window is running.

### Cons
- **It can't answer calls.** It only says a call is ringing. Answering still needs a calling
  client that is already registered (Option A). Without A, the notification can only offer
  "call back", which starts the call window.
- Undocumented protocol: more work, and it may break when Microsoft changes it.
- Endpoint registration is rate-limited (about 5 per hour), so reconnects have to reuse the
  registration.

### Effort
About 1 week, plus upkeep when Microsoft changes the protocol.

---

## Comparison

| | A: hidden call window | B: Trouter |
| --- | --- | --- |
| Answer incoming calls | ✅ | ❌ (notification + call back) |
| "X is calling" notification | ✅ (while running) | ✅ (always) |
| Instant messages / typing / presence | ❌ | ✅ |
| Extra RAM | ~0.5–1 GB while on | negligible |
| Effort | ~1–2 days | ~1 week |
| What can break | Teams' page title or page structure | Trouter protocol |

## Suggested path (if combining)

1. **A first**, opt-in: users who need to answer calls turn on "Receive calls" and accept the
   memory cost.
2. **B later**, for the chat side. Once it's in, it also gives an "X is calling" notification
   when "Receive calls" is off; clicking it calls back through the call window.
