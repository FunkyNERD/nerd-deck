# Custom tabs

A custom tab is a grid of panels on the deck, each showing what a web page, an
API or a command says: an exchange rate, whether a server answers, a reading
from a script, or a note. You can have up to three, beside the built-in tabs.
They're ordered, turned on and off and picked as the start tab on the settings
window's Deck tabs page, like the others.

Build them on the settings window's **Custom tabs** page, or write them in
`~/.config/nerd-deck/config.toml` and choose **Reload config** in the tray.
Either way they end up in the config, as below. Custom tabs need deck firmware
0.11.0 or later (protocol v7); older firmware just doesn't show them.

## How panels are refreshed

- Each panel is refreshed on its own interval: its own `interval`, else its
  tab's. That's separate from the stats interval, so a fast CPU graph never
  means a web request every second.
- The shortest interval is 5 seconds for `url` and `status` panels, and 1
  second for `command` panels.
- Panels on a tab that's turned off aren't refreshed at all, and neither is
  anything while no deck is connected.
- Each refresh runs on a thread of its own, so a slow server or command never
  holds up the stats or a button press.
- Panels that ask the same `url` with the same `headers` share one request.
  Those that are due together wait on one answer. One that falls due while the
  last answer is still younger than its own interval reads that answer
  instead of asking again. So a page several panels read from, like a list of
  exchange rates, is fetched once per refresh, at the rate of its most
  frequent panel.
- The deck only hears about a panel when what it shows changes.
- A panel that can't be refreshed keeps what it showed, greyed, with the
  reason under it (`HTTP 503`, `timed out`, `nothing at /rates/XYZ`). A
  `status` panel shows **down** in red instead.

## Example: an exchange rate

[api.hazaar.dev](https://api.hazaar.dev) has the day's exchange rates for 174
currencies, with no key needed:

```
GET https://api.hazaar.dev/api/money/latest?base=USD
```

```json
{
    "date": "2026-11-04",
    "timestamp": 1793784906,
    "base": "USD",
    "success": true,
    "rates": {
        "AED": 3.6725,
        "AUD": 1.4271,
        "...": "..."
    }
}
```

Every rate is what one unit of `base` is worth in that currency. So for US
dollars to Australian dollars, ask with `base=USD` and read `rates.AUD`:

```toml
[[tab]]
id = "money"
name = "Money"
icon = "refresh"
grid = [2, 1]
interval = 3600            # the rates change daily: hourly is plenty

[[tab.panel]]
kind = "url"
title = "USD -> AUD"
url = "https://api.hazaar.dev/api/money/latest?base=USD"
json = "/rates/AUD"        # the value to show
sub_json = "/date"         # under it: the day the rate is for
decimals = 4
prefix = "$"               # before the number
unit = " AUD"              # and after it

[[tab.panel]]
kind = "url"
title = "AUD -> USD"
url = "https://api.hazaar.dev/api/money/latest?base=AUD"
json = "/rates/USD"
sub_json = "/date"
decimals = 4
prefix = "$"
unit = " USD"
```

The first panel shows something like **$1.4271 AUD**, with **2026-11-04**
under it.

- **Direction:** `base` is the currency you're converting from, and
  `/rates/XXX` the one you're converting to. With `base=AUD` and
  `/rates/USD`, the panel shows AUD -> USD (about 0.70), not USD -> AUD.
- **Other currencies:** use their ISO 4217 codes, like `/rates/EUR`,
  `/rates/GBP` or `/rates/JPY`. A code the API doesn't have shows `nothing at
  /rates/XYZ` under the last value.
- **Several from one base:** panels with the same `url` share one request, so
  `USD -> AUD`, `USD -> EUR` and `USD -> GBP` from `base=USD` cost a single call
  per refresh.
- **Limits:** colour a rate with `warn` and `crit`, say to see when the
  Australian dollar buys less than 0.65 US dollars: `warn = 0.68` and
  `crit = 0.65` on the AUD -> USD panel. With `crit` below `warn`, lower is
  worse.

## Example: an API that needs a key

Many APIs want a key sent as a request header. [API Ninjas](https://api-ninjas.com)
takes one in `X-Api-Key`. Without it, it answers `{"error": "Missing API Key."}`
with HTTP 400, which the panel shows as `HTTP 400` under its last value. Its
exchange rate endpoint gives the rate for a currency pair:

```toml
[[tab.panel]]
kind = "url"
title = "USD -> AUD"
url = "https://api.api-ninjas.com/v1/exchangerate?pair=USD_AUD"
json = "/exchange_rate"
headers = { X-Api-Key = "your key here" }
prefix = "$"
decimals = 4
interval = 3600
```

The key stays on the PC, in `config.toml` as written: it's sent with the
request and never to the deck. Anyone who can read your config can read the
key. The settings window saves the config readable by you only; if you edit it
by hand, `chmod 600 ~/.config/nerd-deck/config.toml` does the same.

In the settings window, a Web value or Status panel has **Request headers**:
**Add header** adds a row named `X-Api-Key`, ready for the key, which is
hidden as you type it. Use **Test** to check the field name in `json`
against what the API really sends.

Panels with the same URL but different headers make separate requests: the
answer may depend on the key.

## Example: a quote

API Ninjas' quotes endpoint answers with a list of quotes, each with its
author. A wide panel shows the quote, wrapped, with the author under it:

```toml
[[tab.panel]]
kind = "url"
title = "Quote"
url = "https://api.api-ninjas.com/v2/quotes"
headers = { X-Api-Key = "your key here" }
json = "/0/quote"          # the first quote in the list
sub_json = "/0/author"
span = [3, 1]              # the full width of a 3-column grid
interval = 3600
```

The more room a panel has, the bigger its text: a quote on a panel two rows
high reads more easily than on one.

## Tabs

```toml
[[tab]]
id = "servers"     # 1-15 letters, digits, - or _; not a built-in tab's name
name = "Servers"   # on the deck's header, up to 15 bytes
icon = "home"      # a symbol name, as for buttons; "list" when left out
grid = [3, 2]      # columns x rows: 1-4 across, 1-3 down
interval = 60      # seconds between refreshes, for panels without their own
```

The deck keeps each tab's place in the order, and whether it's on, by its `id`.
Change the `id` and the deck treats it as a new tab.

Panels fill the grid in the order they're listed, row by row, each in the first
place it fits. A config whose panels don't fit its grid isn't loaded, and says
so.

## Panels

Every panel has a `kind`, and can have:

| Key | Default | |
|---|---|---|
| `title` | none | Above the value, up to 23 bytes. |
| `span` | `[1, 1]` | Columns and rows it covers. |
| `interval` | the tab's | Seconds between refreshes. |
| `timeout` | `10` | Seconds to wait for an answer, 1-60. |

### `url`: a value from a web page or API

| Key | |
|---|---|
| `url` | Fetched with GET. Anything but a 2xx answer is an error. |
| `json` | A [JSON Pointer](https://www.rfc-editor.org/rfc/rfc6901) to the value, like `/rates/AUD` or `/data/0/price`. Without it, the panel shows the first line of the response that isn't blank. |
| `sub_json` | A JSON Pointer to a note shown under the value. |
| `headers` | Sent with the request, say an API key: `headers = { Authorization = "Bearer ..." }`. They stay on the PC; the deck never sees them. |
| `prefix`, `unit`, `decimals`, `warn`, `crit` | See [Numbers](#numbers). |

### `status`: whether a web address answers

| Key | |
|---|---|
| `url` | Fetched with GET. |
| `slow_ms` | `1000`: slower than this, the dot turns orange. |
| `status_text` | `false`: when `true`, shows the status's reason, like **OK** or **Not Found**, in place of its code. |
| `headers` | As for `url`. |

It shows the status code (or its reason, with `status_text`) and how long the answer took. The dot is green, orange
when slow, and red for a 4xx or 5xx code or no answer at all.

```toml
[[tab.panel]]
kind = "status"
title = "Forgejo"
url = "https://git.hazaar.dev"
interval = 30
```

### `command`: the first line a command prints

| Key | |
|---|---|
| `command` | Run with `sh -c`, as you, like a button's command. |
| `prefix`, `unit`, `decimals`, `warn`, `crit` | See [Numbers](#numbers). |

The panel shows the first line of output that isn't blank. A command that exits
with an error shows as failed. One that runs past its `timeout` is killed,
along with anything it started.

```toml
[[tab.panel]]
kind = "command"
title = "Load"
command = "cut -d' ' -f1 /proc/loadavg"
decimals = 2
warn = 4
crit = 8
interval = 5

[[tab.panel]]
kind = "command"
title = "Root free"
command = "df --output=pcent / | tail -1 | tr -dc 0-9"
unit = "%"
warn = 80
crit = 95
```

### `text`: text of your own

```toml
[[tab.panel]]
kind = "text"
title = "Note"
text = "Bins out Tuesday night"
```

## Numbers

When a `url` or `command` panel's value is a number:

- `decimals`: how many digits to show after the point. Left out, the number
  shows as it comes.
- `prefix` goes before the number and `unit` after it, exactly as written,
  so put any space in them: `prefix = "$"` shows `$1.43`, `unit = " mm"` shows
  `12 mm`, and both together show `$1.43 AUD`. Either can be left out.
- `warn` and `crit`: from `warn` the value shows orange, and from `crit` red.
  When `crit` is below `warn`, lower is worse: the value is orange at or below
  `warn`, and red at or below `crit`.

The deck's fonts are ASCII only, so `°`, `€` or `→` in a value, a prefix or a
unit are left out. Write `C`, `EUR` or `->` instead.

## On the deck

- A `url` or `command` panel shows its value as big as it fits on one line,
  with the note in small text under it. A value too long for one line, like a
  quote, wraps under the title instead, as big as it fits; whatever still
  doesn't fit ends in `...`.
- A `status` panel adds a coloured dot beside its title.
- A `text` panel wraps its text.
- Values are cut to 255 characters, and notes to 31.
- With more than five tabs shown, the header's tab pills get narrower. With all
  eight, the open tab's pill shows just its icon.

## In the settings window

On the **Custom tabs** page:

- **Add tab** makes a tab. On the right, set its name, icon, grid and refresh
  interval.
- Click an empty space in the preview to add a panel, or a panel to change it:
  its kind, the fields for that kind, its size, its number options (what goes
  before and after the value, decimals, and limits) and its refresh interval.
- **Test** fetches a panel once, and the preview shows what it would say.
- Changes are written to `config.toml` straight away, keeping its comments, and
  the deck follows within a moment. A change that wouldn't fit the grid is
  refused, with a message saying why.

`timeout` and `slow_ms` are only set in the config file. The settings window
keeps them as they are.

## Protocol

How custom tabs travel to the deck (the `tabs`, `tab` and `panel` messages) is
in [PROTOCOL.md](PROTOCOL.md).
