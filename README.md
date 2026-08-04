# fsctl

Een bestandsbeheerder voor de terminal, met twee kolommen: links een **bron**,
rechts de items daarvan. De mappenboom is maar één van de bronnen — git weet
welke repo's er zijn en wat er niet opgeslagen is, en dat levert twee extra
perspectieven op dezelfde bestanden zonder een tweede manier van tekenen.

**Nul dependencies.** De terminal-laag is eigen werk (overgenomen uit sshctl),
en al het echte werk gaat naar het systeem: `cp` en `mv` verplaatsen de bytes,
`git status` kent de repo's, `open` opent bestanden, `stty` zet de terminal in
raw mode.

## Installeren

```sh
cargo build --release
ln -sf "$PWD/target/release/fsctl" /opt/homebrew/bin/fsctl
```

### Meelopen met je shell

`q` schrijft de map waar je stond weg, zodat je shell erheen kan springen. Zet
dit in `~/.zshrc`:

```sh
f() {
  local out="$(mktemp)"
  FSCTL_CWD_FILE="$out" command fsctl "$@"
  local dir="$(cat "$out")"; rm -f "$out"
  [ -n "$dir" ] && [ -d "$dir" ] && cd "$dir"
}
```

Daarna is `f` je bestandsbeheerder, en waar je hem verlaat, sta je.

## Toetsen

| | |
|---|---|
| `1` `2` `3` | van bron wisselen |
| `Tab` | van kolom wisselen |
| `j` `k` · pijltjes | omhoog en omlaag |
| `l` `→` | map uitklappen, of naar rechts |
| `h` `←` | map dichtklappen, of terug naar links |
| `Enter` | bestand openen met `open` |
| `w` | de map onder de cursor wordt de wortel van de boom |
| `W` | de wortel één map omhoog |
| `spatie` | bestand aan- of afvinken |
| `c` `m` `v` | kopiëren · knippen · plakken |
| `p` | in het bestand kijken (`j`/`k` op en neer, `d`/`f` zijwaarts, `0` terug) |
| `x` `Del` | naar de prullenbak (vraagt eerst) |
| `s` `u` | sorteren (naam/type/datum) · omkeren |
| `.` | verborgen bestanden tonen |
| `r` | verversen |
| `Esc` | selectie wissen, daarna het klembord |
| `?` | de hulp, met alles wat hier staat |
| `q` | sluiten |

In de boom staat vóór elke map wat je ervan mag verwachten:

```
▾  uitgeklapt            ▸  er zitten mappen in
×  écht helemaal leeg    ·  ziet er leeg uit, maar bevat verborgen inhoud
(niets)  alleen bestanden — en die staan rechts
```

Het driehoekje en het kruisje beantwoorden bewust **verschillende** vragen. Het
driehoekje gaat over wat uitklappen zou tonen, dus het volgt je `.`-instelling —
een driehoekje dat opengaat op niets is een leugen. Het kruisje gaat over wat er
werkelijk staat, verborgen bestanden meegeteld, want `cp` en `mv` werken op de
map zoals die op schijf staat en niet op onze gefilterde weergave. Een map met
alleen een `.env` erin mag dus nooit "leeg" heten; die krijgt een punt.

**Mappen links, bestanden rechts.** De boom is de enige plek waar een map
staat; de rechterkolom toont er geen. Een hele map kopiëren doe je dus met de
cursor in de boom: `c` of `m` pakt daar de map waar je op staat. In de
rechterkolom werken ze op wat je hebt aangevinkt, of anders op de regel waar je
staat.

## De bronnen

- **Mappen** — de gewone boom, natuurlijke sortering (`v2` vóór `v10`),
  symlinks als symlink.
- **Repo's** — elke git-repo onder je zoekpaden, met tak, aantal wijzigingen en
  ↑↓ tegenover de remote.
- **Onopgeslagen** — alleen de repo's mét wijzigingen; rechts elk gewijzigd of
  ongetrackt bestand.

In elke mapweergave die binnen een repo valt, krijgen de bestanden hun
git-staat als kolom. Dat kost geen extra `git`-aanroep: de status is al binnen.

Zoekpaden zijn standaard `~/Development`. Overschrijven kan met `FSCTL_ROOTS`,
dubbelepunt-gescheiden zoals een `PATH`:

```sh
FSCTL_ROOTS="$HOME/Development:$HOME/Werk" fsctl
```

## Wat het aan het systeem overlaat

| handeling | commando | waarom |
|---|---|---|
| kopiëren | `/bin/cp -Rc`, terugval `-R` | behoudt xattrs, symlinks en rechten; `-c` kloont op APFS (ogenblikkelijk, geen extra schijfruimte) |
| verplaatsen | `/bin/mv -f` | regelt de volumegrens zelf |
| openen | `/usr/bin/open` | macOS weet welke app erbij hoort |
| repo's vinden | `/usr/bin/find` | één proces over honderdduizenden bestanden |
| repo-staat | `git --no-optional-locks status --porcelain --branch` | per repo, nooit per bestand |
| tijdzone | `/bin/date +%z` | goedkoper dan zelf `/etc/localtime` lezen |

Argumenten gaan rechtstreeks naar het proces, nooit via een shell. Een bestand
dat `; rm -rf ~` heet is dus gewoon een bestand met een ongelukkige naam.

## Kopiëren, en wat er bij een botsing gebeurt

`cp -R` **voegt mappen samen**; het vervangt ze niet. Een bestand dat alleen op
de bestemming bestond, blijft dus staan — Finder gooit in datzelfde geval de
hele doelmap weg. Die veiligere semantiek erven we gratis door het werk uit
handen te geven.

Botsende namen worden vóóraf geteld, niet onderweg ontdekt: je krijgt één vraag
met het volledige overzicht in plaats van zeven onderbrekingen.

- **[B] Beide bewaren** — de aankomst wordt `naam-2`; niets op de bestemming
  wordt aangeraakt
- **[O] Overschrijven** — mappen worden samengevoegd, gelijknamige bestanden
  vervangen
- **[S] Overslaan** — de botsers blijven staan, de rest gaat door
- **[Esc]** — afbreken

## Een blik in een bestand

`p` opent het bestand onder de cursor in een venster: genummerde regels,
scrollen met `j`/`k`, `PgUp`/`PgDn` en `g`/`G`, en **zijwaarts** met `d`/`f` in
stappen van acht kolommen (`0` schuift terug naar het begin; `h`/`l` en de
pijltjes doen hetzelfde). Lange regels
worden dus afgekapt en niet omgebroken — code blijft leesbaar, en wat erbuiten
valt schuif je in beeld. Bedoeld om te bevestigen dat je
het juiste bestand te pakken hebt, niet om in te lezen — daarvoor is `Enter`,
dat de gewone app opent.

Of iets tekst is, wordt bepaald door de **inhoud** en niet door de extensie:
een `Makefile` of `.zshrc` heeft er geen, en een `.log` kan best binair zijn.
Een nulbyte in de eerste 128 KB betekent geen tekst, en dan zegt het venster
dat gewoon. Meer dan 128 KB wordt niet gelezen; loopt het bestand door, dan
staat dat onderaan.

## Verwijderen

Naar de prullenbak, nooit rechtstreeks weg. Finder doet het via `osascript`,
want de put-back-informatie voor "Zet terug" wordt vastgelegd door wie het
verplaatst — en dat is alleen Finder. Paden reizen als argumenten, niet in de
scripttekst, zodat een aanhalingsteken in een naam geen deel van het programma
kan worden.

De vraag vooraf zegt wat het kost: hoeveel items, hoeveel mappen, en — het
enige wat je scherm je niet kon vertellen — **welke mappen verborgen inhoud
bevatten die mee weggaat**. Dat is dezelfde `·` uit de boom, nu als
waarschuwing.

Weigert Finder (automatisering niet toegestaan, Finder bezig), dan gaan de
bestanden alsnog naar `~/.Trash`, met de hand verplaatst. Terug te halen, maar
zonder "Zet terug"; de statusregel zegt het erbij. Wil je Finder er helemaal
buiten houden: `FSCTL_TRASH=plain`.

De wortel van de boom kan niet weg — daar zou de weergave op stukvallen.

## Grenzen van v0.1

- **Geen voortgangsbalk.** Een APFS-kloon is ogenblikkelijk; een echte kopie
  over een volumegrens laat de tool even stilstaan. `ditto -V` kan dat later
  voeden.
- **Trage repo's worden overgeslagen.** Gemeten op deze machine: een gewone
  repo antwoordt in ~0,1 s, maar een gearchiveerde met een grote ongetrackte
  boom deed er 209 s over. Na anderhalve seconde stopt fsctl met wachten en
  toont "te traag — niet gelezen". De repo blijft in de lijst staan.
- **Geen achtergronddienst.** Een sweep over 26 repo's kost een paar seconden;
  `r` ververst alleen wat sinds de vorige keer bewoog (afgemeten aan
  `.git/index` en `.git/HEAD`).
- **Tekenbreedte is een compacte tabel**, geen volledige Unicode-database. Een
  zeldzaam teken kan één cel uitlijning kosten, nooit meer dan dat.

## Ontwerp

Zie [DESIGN.md](DESIGN.md) — inclusief de gemeten cijfers waarop de keuzes
rusten, en wat er bewust *niet* gebouwd is (handmatig taggen, een daemon,
metadata als xattr op je bestanden).
