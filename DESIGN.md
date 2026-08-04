# fsctl — ontwerp

*Werknaam. Zero-dependency TUI-bestandsbeheerder in de lijn van sshctl.*
Vastgelegd 2026-08-04 na de ontwerpsessie.

## Waarom

Finder vervangen als dagelijkse bestandsbeheerder, om twee redenen: hij schrijft
overal `.DS_Store` (macOS 26.6 kent maar één schakelaar, `DSDontWriteNetworkStores`
— empirisch geverifieerd in de dyld shared cache; `DSDontWriteUSBStores` en
`DSDontWriteStores` bestaan niet), en hij kent maar één perspectief: de mappenboom.

## Doctrine

1. **Nul dependencies.** Zoals sshctl. Geen ratatui, geen crossterm, geen libc-crate.
2. **De instantie die het weet, doet het werk.** sshctl vraagt `ssh -G` in plaats van
   tekst te vergelijken. Hier: `cp`/`mv` verplaatsen bytes, `git` kent de repo-staat,
   `plutil`/`xattr` lezen metadata. Wij orkestreren, wij herimplementeren niet.
3. **Geen handmatige metadata.** Taggen met de hand werkt niet in de praktijk.
   Alles wat we tonen is afgeleid — of het bestaat niet.
4. **De index is een cache, nooit de waarheid.** Weggooien mag altijd; herbouwen
   is seconden werk.

## Hergebruik uit sshctl

`publish/src/bin/sshctl-tui/term.rs` (1.247 regels) is een zero-dep terminal-laag met
een ratatui-vormige API: `Color` `Style` `Span` `Line` `Text` `Rect` `Constraint`
`Layout` `Block` `Paragraph` `Tabs` `Clear` `Frame` `DefaultTerminal` `Event` `KeyCode`.
Raw mode via een `stty`-subprocess — geen FFI, geen `unsafe`.

**Kopiëren, niet uitfactoren** naar een gedeelde crate: twee onafhankelijke tools
blijven twee onafhankelijke tools. Verbeteringen vloeien met de hand terug.

Twee aanvullingen nodig:

- **Lijst-widget** met cursor, selectie en scroll (~150 regels). Dient zowel de boom
  links als de lijst rechts.
- **Tekenbreedte** (~80 regels). `term.rs` rekent één kolom per teken (regel 351).
  Klopt voor hostnamen, niet voor bestandsnamen met emoji of CJK.

## Scherm

```
┌─ bronnen ────────┬─ items ─────────────────────────┐
│ 📁 Mappen        │ ▣ naam            type    datum │
│ 📦 Repo's        │ ▢ …                             │
│ 🔥 Onopgeslagen  │                                 │
└──────────────────┴─────────────────────────────────┘
```

Links een **bron** (een boom), rechts altijd "de items van de geselecteerde knoop".
Eén weergavepad, meerdere bronnen. Sorteren op naam, type of wijzigingsdatum;
mappen eerst. Selecteren met spatie, getoond als ▣/▢.

## Systeemdelegatie (empirisch geverifieerd, macOS 26.6)

| handeling | commando | bewezen |
|---|---|---|
| kopiëren | `/bin/cp -Rc` → terugval `-R` | xattrs, symlinks én rechten blijven; `-c` = APFS-kloon (instant, geen extra ruimte) |
| verplaatsen | `/bin/mv` | regelt de volumegrens zelf; geen EXDEV-code nodig |
| repo-staat | `git status --porcelain --ignored`, `git ls-files` | 0,095 s per repo; **per repo, nooit per bestand** |
| cache geldig? | `stat` op `.git/index` + `.git/HEAD` | onveranderd = git niet draaien |
| voortgang (later) | `ditto -V` | schrijft per bestand een regel, live mee te lezen |
| prullenbak (later) | `osascript` → Finder | levert "Zet terug" gratis |

Absolute paden gebruiken (`/bin/cp`, niet `cp`). Rust' `Command` geeft argumenten
zonder shell door — bestandsnamen met spaties of newlines zijn geen risico.

**Valstrik:** `ditto src dst` kopieert de *inhoud* van `src` naar `dst`, niet de map
zelf. `cp -R` doet dat wel. Bij gebruik van ditto zelf `dst/naam` samenstellen.

**Conflicten:** geen enkele systeemtool kan "vragen bij conflict"; `cp -n` slaat stil
over en geeft tóch exitcode 0. Bestaan dus zelf controleren vóór de aanroep, keuze
aan de gebruiker, dan pas het juiste commando. De beslissing is UI, het verplaatsen
is systeemwerk.

## Metadata: alles afgeleid

Leveranciers van feiten, elk met hun eigen kolommen:

- **git** — repo, tak, getrackt?, gewijzigd?, genegeerd?, laatste commit
- **bestandssysteem** — naam, type, grootte, mtime, rechten
- **macOS** — `kMDItemWhereFroms` (bron-URL van downloads), `kMDItemDownloadedDate`,
  `com.apple.lastuseddate#PS` (laatst geopend). Automatisch geschreven door het
  systeem; niemand hoeft iets te taggen.

Handmatige velden (Finder-tags via `com.apple.metadata:_kMDItemUserTags`,
Finder-opmerking via `kMDItemFinderComment`) zijn technisch bewezen werkend via
`plutil` + `xattr` + `xxd`, en overleven `cp` en `mv` — maar worden **niet gebouwd**.
In heel `~/Development` (465.061 bestanden) staat vandaag geen enkele tag.

**Afgeleide data komt nooit als xattr op bestanden.** Dan worden wij de nieuwe
`.DS_Store`: duizenden bestanden bekladden met gegevens die morgen niet kloppen.
Alles wat afgeleid is, leeft in de index.

### Indexformaat

Eén regelgebaseerd bestand, met de hand geparsed zoals sshctl de ssh-config parseert:

```
dev:inode ⇥ pad ⇥ mtime ⇥ grootte ⇥ type ⇥ git-staat
```

`dev:inode` erbij omdat het inode-nummer op APFS gelijk blijft bij hernoemen en
verplaatsen binnen een volume: een bestand dat buiten onze tool om verhuist, wordt
herkend en de index heelt zichzelf.

## v0.1

- 📁 Mappen — boom links, bestanden rechts
- 📦 Repo's — alle git-repo's met tak en dirty-telling
- 🔥 Onopgeslagen werk — elk gewijzigd bestand over alle repo's heen
- sorteren (naam/type/datum), selecteren met spatie, copy/cut/paste
- cd-on-exit: pad naar een bestandje, shell-functie doet de `cd`

Gemeten: 25 repo's onder `~/Development`, gevonden in 0,027 s; volledige sweep ≈ 2,5 s.
**Geen achtergronddienst.** Lui berekenen bij het openen van een map is onmerkbaar;
een launchd-agent verdient zijn plaats pas als de globale lijst ogenblikkelijk moet
zijn of meldingen moet geven. v0.2-beslissing, met echte ervaring.

Schatting: ~750 regels bovenop `term.rs`, plus ~350 voor de index en de git-bronnen.

## Open

- **Naam.** `fsctl` is een werknaam.
- **Paste bij naamconflict**: vragen (met "op alles toepassen"), automatisch
  hernoemen naar `naam-2`, of overslaan?
- **Selectie globaal of per map?** Globaal is krachtiger — aanvinken in drie mappen,
  in één keer plakken — maar vraagt dat je toont hoeveel er buiten beeld staat.
- **Symlinks bij kopiëren**: de link meenemen (advies) of het doel uitschrijven.
- **Verwijderen** stond niet in de wensenlijst. Later, en dan naar `~/.Trash`.

## v0.2 en verder

Filters als vierde bron (opgeslagen query's over de index) · `❓ in een repo maar niet
in git` · `🚫 genegeerd` · voortgangsbalk via `ditto -V` · prullenbak via osascript ·
launchd-sweep · breedtetabel voor emoji en CJK.
