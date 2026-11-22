# Translating Siler

Siler's words live in two catalogs. English is the source and the fallback: anything a
translation leaves out is shown in English.

| What | File | Format |
| --- | --- | --- |
| The app: window, tray, notifications, errors | `app/locales/<lang>/siler.ftl` | [Fluent](https://projectfluent.org/fluent/guide/) |
| The browser extension | `extension/_locales/<lang>/messages.json` | [WebExtension i18n](https://developer.mozilla.org/docs/Mozilla/Add-ons/WebExtensions/Internationalization) |

No translations exist yet; English (`en-US` for the app, `en` for the extension) is the only
language.

## Adding a language to the app

1. Copy `app/locales/en-US/siler.ftl` to `app/locales/<lang>/siler.ftl`, where `<lang>` is a
   BCP 47 tag such as `de`, `fr`, `pt-BR` or `zh-CN`.
2. Translate the text after each `=`. Keep the ids (left of `=`) and the variables
   (`{ $name }`) as they are. Variables carry numbers, names and sizes.
3. Plurals and other choices use selectors. Give your language's plural categories (`one`,
   `few`, `many`, `other`…); one variant must be the default (`*`):

   ```ftl
   main-links-copied =
       { $count ->
           [one] Link copied
          *[other] { $count } links copied
       }
   ```

4. Add the language to the Rust side in `app/src-tauri/src/i18n.rs`. `CATALOGS` builds the
   file into the binary:

   ```rust
   const CATALOGS: &[(&str, &str)] = &[
       ("en-US", include_str!("../../locales/en-US/siler.ftl")),
       ("de", include_str!("../../locales/de/siler.ftl")),
   ];
   ```

   The window finds the new file by itself.
5. Run `node app/locales/check.mjs`. It reports messages that aren't in English (stale ids),
   variables English doesn't pass, and how many messages are still in English.

Users choose the language in **Settings → General → Language**; by default Siler follows the
system (`LANG`). The choice takes effect when Siler starts again.

### Finding text that isn't in the catalog

Development builds offer a pseudo-language, **en-XA**, in Settings → General → Language.
With it, every message from the catalog shows accented and in brackets (`[Ŝíléř]`), so any
plain English text is text the catalog doesn't have yet.

A few messages carry English from deep inside the engine: a server's own words, or details
such as "the server doesn't send parts of files" (`error-text`). These stay English for now.

## Adding a language to the extension

1. Copy `extension/_locales/en/messages.json` to `extension/_locales/<lang>/messages.json`
   (`de`, `fr`, `pt_BR`, `zh_CN`: an underscore, not a hyphen).
2. Translate each `message`; leave the keys and `$1`, `$2` (the values put in) as they are.
3. Run `node --test extension/tests/*.test.mjs`.

The browser picks the language itself, from its own interface language.

## Conventions

- Write as the English does: plain, short sentences that say what happens. Don't add
  exclamation marks.
- Keep "Siler" as it is. Keep product and protocol names as well (FFmpeg, HLS, SFTP, YouTube).
- Use the typographic characters the English uses (“ ” … – ·), adjusted to your
  language's usual quotation marks.
