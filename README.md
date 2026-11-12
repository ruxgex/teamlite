# Teamlite

A fast, native Microsoft Teams **chat** client for Linux, written in Rust and drawn with
[Rune](https://github.com/anomalyco/rune) (GPU UI, no browser engine in the main window).

Built for personal use as a lighter alternative to the Teams PWA. Calls and meetings are not
supported — open those in the browser.

## Features

- Chats, group chats, meeting chats and channels; pinned chats on top
- Live updates every 2–5 s (chat-service delta sync), unread badges, desktop notifications
- **Multitenancy**: every organisation you belong to (home + guest tenants) in the left rail,
  each with its own live connection and unread count; switch with a click or `Ctrl+1…9`
- Send messages (Enter to send, Shift+Enter for a new line), formatted message rendering
  (bold, italic, code, lists, mentions, links, reply quotes)
- Inline images and GIFs (Teams AMS images are fetched with your session)
- Send files and images: **+ Attach** (desktop file picker) or paste a screenshot with Ctrl+V.
  Images go inline; other files go to your OneDrive "Microsoft Teams Chat Files" folder,
  shared read-only with the chat's members, exactly as Teams does (some organisations block
  OneDrive access for this app; Teamlite then says so). Received files show as chips.
- Reactions (👍 ❤️ 😆 😮 😢 😡): see who reacted, add or remove yours
- Edit and delete your own messages (delete asks for a second click), copy any message
- Channel reply threads: open a post's replies and answer in the thread
- Presence dots and status for 1:1 chats; others see "typing…" while you write
- **Call / Video** (1:1) and **Open in Teams** buttons hand calls, meetings and files to Teams on
  the web through its public deep links
- Instant start: chat list and recent messages cached locally
- Keyboard: `Ctrl+K` search, `Alt+↑/↓` previous/next chat, `Esc` closes menus, cancels edit/reply

Not native: calls and meetings (the header buttons open them in Teams on the web),
and *seeing* others type (that needs Trouter, Teams' websocket, which isn't implemented).

## How it works

Teamlite talks to the same services as teams.microsoft.com/v2:

| Step | Endpoint |
| --- | --- |
| Sign-in | Azure AD auth code + PKCE with the Teams desktop client id, in a small WebKitGTK window |
| Skype token | `POST teams.microsoft.com/api/authsvc/v1.0/authz` |
| Chats / messages | `{chatService}/v1/users/ME/conversations[/{id}/messages]` |
| Live updates | delta sync: `GET` the chat list's `_metadata.syncState` URL, which returns only changed chats |
| Organisations | `GET {middleTier}/beta/users/tenantsv2`; a token per tenant from the same refresh token |
| Names | `POST {middleTier}/beta/users/fetchShortProfile` |
| Reactions | `PUT/DELETE …/messages/{id}/properties?name=emotions` |
| Edit / delete | `PUT …/messages/{id}`, `DELETE …/messages/{id}?behavior=softDelete` |
| Channel replies | conversation id `{channel};messageid={root}` |
| Images | `GET` the AMS URL with `Authorization: skype_token …` |
| Presence | `POST presence.teams.microsoft.com/v1/presence/getpresence/` |
| Sending files | Graph `PUT /me/drive/root:/Microsoft Teams Chat Files/{name}:/content` (upload sessions over 4 MB), `invite`, message `properties.files` |
| Sending images | `POST {ams}/v1/objects/` then `PUT …/content/imgpsh`, message with `amsreferences` |

These are undocumented and may change. Tokens live in `~/.config/teamlite/account.json`
(mode 0600); caches in `~/.cache/teamlite/`. `teamlite --sign-out` removes both.

An organisation that demands its own MFA shows a *Sign in to this organisation* button.
The old long-poll endpoint answers 410 and endpoint registration is limited to 5/hour, so
Teamlite doesn't use it; Trouter (the web client's websocket) isn't implemented yet.

If your organisation blocks the Teams desktop client id, set `TEAMLITE_CLIENT_ID` and
`TEAMLITE_REDIRECT_URI`.

## Build

```sh
cargo build --release          # needs libwebkit2gtk-4.1-dev, libxkbcommon-dev, libwayland-dev
install -Dm755 target/release/teamlite ~/.local/bin/teamlite
install -Dm644 packaging/teamlite.desktop ~/.local/share/applications/teamlite.desktop
```

## Testing the UI headlessly

```sh
teamlite --snap out.png --scene demo            # scenes: signin, demo, offline, search
teamlite --scene demo --script scripts/send.json --snap out.png --at mark:end
cargo test
```

Live checks against your own account (shapes only, no message content printed):

```sh
teamlite --probe [tenant-id]
TEAMLITE_E2E_IMAGE=pic.png \
teamlite --e2e "<org>" "<chat>" "message to send"      # send, echo, react, edit, delete, deep link, image upload, presence
teamlite --e2e "<org>" "<chat>" "listen:<text>"        # waits for a message from another client
teamlite --e2e "<org>" "<channel>" "replies:"            # opens a channel reply thread (read-only)
teamlite --e2e "<org>" "<chat>" "cleanup:<text>"         # deletes your messages containing <text>
teamlite --e2e-tenants                                   # switches through every organisation twice
```
