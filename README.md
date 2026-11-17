# Roadmap

Rustban írt Linuxos alkalmazás meghajtók és partíciók listázásához, valamint SMART-adatok olvasásához. A fordításhoz Rust és Cargo 1.75 vagy újabb, a futtatáshoz `lsblk`, `timeout` és `smartctl` szükséges. Ubuntu alatt ezeket a `util-linux`, a `coreutils` és a `smartmontools` csomag biztosítja.

## Grafikus felület

Ubuntu 24.04 alatt a grafikus változathoz `libgtk-4-1` (GTK 4.6 vagy újabb) és a szükség szerinti hitelesítéshez `pkexec` kell. Indítsd a programot a saját felhasználóddal.

```bash
cargo build --release --locked
./target/release/roadmap --gui
```

Válassz meghajtót a bal oldali listában, majd kattints a **SMART lekérése** gombra. Az összefoglaló, a teljes szöveges jelentés és a JSON külön fülön érhető el. Ha a meghajtóhoz magasabb jogosultság kell, a rendszer a SMART-lekérdezéshez kér hitelesítést.

A kiválasztott meghajtó hőfoka a neve alatt jelenik meg és öt másodpercenként frissül, amíg az ablak nyitva van. A Roadmap a Linux hőfokszenzorát vagy normál jogosultságú SMART-lekérdezést használ. Ha az élő érték nem érhető el, az utolsó ismert hőfokot külön megjelöli, és 30 másodperc múlva próbálkozik újra. A meghajtólistában a legutóbbi SMART-mérés állapota és időpontja is látható.

A **Pillanatkép mentése** gomb meghajtónként menti a lekért fő adatokat a `$XDG_DATA_HOME/roadmap/history` mappába. Az alapértelmezett hely a `~/.local/share/roadmap/history`. A következő SMART-lekéréskor az előző mentéshez képest látható, mely értékek változtak. A növekvő hibaszámlálók külön jelzést kapnak, ha mindkét mérés közli a számlálót. Az **Előzmények** fülön az utolsó 50 mentés látható.

A **Naplók** fül a meghajtó jelentésében szereplő hiba- és öntesztnaplókat jeleníti meg. A **Jelentés mentése…** szövegfájlt készít a választott helyen. Az exportban a sorozatszám alapból rejtett, a meglévő fájlok felülírása helyett új fájlnevet kér.

A meghajtó neve alatt színes jelző mutatja a SMART-kondíciót. A zöld megfelelő állapotot, a sárga adathordozó-hibákat vagy legfeljebb 10% hátralévő élettartamot, a piros kritikus SMART-jelzést, a szürke hiányzó állapotadatot jelent. A százaléksáv az SSD által közölt kopásadatból számolt, becsült hátralévő névleges élettartamot mutatja. HDD-knél és kopásadat nélküli SSD-knél a kondíciójelző mellé a százalék hiánya kerül.

Az alkalmazásmenü-bejegyzés és a Roadmap ikon telepítéséhez futtasd az alábbi parancsot. A menüpont a lefordított program jelenlegi elérési útjára mutat.

```bash
./target/release/roadmap --install-desktop
```

## Terminálfelület

```bash
cargo build --release --locked
sudo ./target/release/roadmap
```

GTK nélküli gépen a terminálváltozathoz használd a `cargo build --release --locked --no-default-features` parancsot.

Válassz meghajtót a fel/le nyilakkal, majd nyomj Entert a SMART-adatok lekéréséhez. Az `1` az összefoglalót, a `2` a teljes jelentést, a `3` pedig a JSON-nézetet nyitja meg. Az `R` újra listázza a meghajtókat, az `U` USB-adaptertípust vált, a `Q` kilép.

Az összefoglaló mutatja az elérhető gyártói és modelladatokat, a sorozatszámot, a kapacitást, a firmware-t, az állapotot, a hőmérsékletet, az üzemidőt, a bekapcsolási és motorciklusokat, a szabálytalan leállásokat, az összes írt és olvasott hostadatot, valamint a hibaszámlálókat. A teljes jelentésben a meghajtó által kiadott további mezők, attribútumok és naplók is megjelennek.

A hiányzó számlálók helyén „–” áll. Az ATA gyártóspecifikus írási attribútumait ismeretlen mértékegység esetén nyersen jeleníti meg. A bekapcsolási ciklus, a motor indítás/leállítás és a szabálytalan leállás külön számláló. Az összes írt hostadat a gép által a meghajtónak küldött adatmennyiség.

Parancssorból közvetlenül is lekérheted ugyanezeket az adatokat. A `--type sat` egy SAT-átvitelt támogató USB-adapterhez használható.

```bash
./target/release/roadmap list
sudo ./target/release/roadmap smart /dev/nvme0n1
sudo ./target/release/roadmap smart /dev/nvme0n1 --json
sudo ./target/release/roadmap smart /dev/sdX --type sat --full
```

A program műveletei a meghajtók listázására és a SMART-jelentések kiolvasására korlátozódnak. A `smart` parancs érvényes jelentés esetén a `smartctl` státuszkódját adja vissza. Részben elérhető jelentésnél is megjeleníti a kapott adatokat.
