# Roadmap

**Magyar** · [English](README.en.md)

A Roadmap egy Rustban írt Linuxos eszköz HDD-k és SSD-k ellenőrzéséhez. A grafikus felületen egy helyen látszik a meghajtó kondíciója, hőmérséklete, üzemideje és az eddig írt adat. Terminálos felület és szkriptekből használható parancsok is elérhetők.

## Gyors kezdés

A fordításhoz Rust és Cargo 1.75 vagy újabb, a grafikus felülethez GTK 4.6 vagy újabb szükséges. Ubuntu 24.04-en a futtatási függőségek így telepíthetők.

```bash
sudo apt install util-linux coreutils smartmontools libgtk-4-1 pkexec
cargo build --release --locked
./target/release/roadmap --gui
```

Az ablakot saját felhasználóval indítsd. A Roadmap szükség esetén a SMART-lekérdezéshez kér rendszergazdai hitelesítést. Az alkalmazásmenübe a következő paranccsal veheted fel. A bejegyzés a bináris jelenlegi helyére mutat.

```bash
./target/release/roadmap --install-desktop
```

## Grafikus felület

1. Válassz egy meghajtót a bal oldali listában, majd kattints a **SMART lekérése** gombra.
2. Az összefoglaló mutatja a kondíciót és a figyelmeztetés okát. Az **Állapot** százalék akkor jelenik meg, ha a meghajtó közöl élettartamra vonatkozó kopásadatot. A mező az ebből számolt, becsült hátralévő névleges élettartamot mutatja.
3. A **Naplók** fül a meghajtó által közölt hiba- és öntesztnaplót mutatja. A teljes SMART-kimenet a **Teljes jelentés** és **JSON** füleken érhető el.

A kiválasztott meghajtó élő hőfoka öt másodpercenként frissül, amíg az ablak nyitva van. Ha az élő olvasás sikertelen, a Roadmap „utolsó ismert” jelöléssel mutatja a korábbi értéket, ha van ilyen, és 30 másodperc múlva újrapróbálkozik. A meghajtólista a legutóbbi elérhető SMART-mérés állapotát és idejét is mutatja.

A kondíciójelző zöld, ha az állapot megfelelő, sárga adathordozó-hiba vagy legfeljebb 10% hátralévő névleges élettartam esetén, piros kritikus SMART-jelzésnél, szürke pedig hiányzó állapotadatnál. A lekérdezés csak olvassa a meghajtó SMART-adatait.

### Előzmények és állapotgrafikon

A **SMART lekérése** után a Roadmap automatikusan menti az azonosítható meghajtó mérését. Az előzmények helye alapból `~/.local/share/roadmap/history`, beállított `XDG_DATA_HOME` esetén `$XDG_DATA_HOME/roadmap/history`.

Az **Előzmények** fül az utolsó 50 mérést használja. Két mentett méréstől megjelenik a grafikon. Ha legalább két mérésben van százalékos adat, az **Állapot** görbéje látszik. Egyébként a grafikon a SMART-kondíciót követi. A narancssárga pont az előző méréshez viszonyított romlást jelöli. A pont fölé húzva az egérmutatót látható a mérés ideje. A grafikon alatt két mérés egymás mellett összevethető. Alapból az eltérések látszanak, a **Minden érték** kapcsolóval a többi adat is megjelenik.

### Mérési lap és átvitel másik gépre

- A **Mérési lap mentése…** kompakt PNG-t készít a fő adatokkal. A sorozatszám alapból rejtett. A **Sorozatszám feltüntetése** kapcsolóval rátehető a képre.
- Az **Aktuális mérés exportálása…** hordozható JSON-fájlt készít. Másik gépen kérj friss SMART-mérést ugyanarról a meghajtóról, majd válaszd az **Előzmények → Mérés importálása…** lehetőséget. Az egyezést a Roadmap a sorozatszám és a kapacitás alapján ellenőrzi. A JSON ezért tartalmazza a sorozatszámot.

## Terminálos használat

```bash
cargo build --release --locked --no-default-features
./target/release/roadmap
```

A terminálos buildhez a grafikus függőségek elhagyhatók. Ha a meghajtó SMART-adatainak kiolvasásához magasabb jog szükséges, a terminálos programot vagy a `smart` parancsot futtasd `sudo`-val.

| Billentyű | Művelet |
| --- | --- |
| ↑ / ↓ | Meghajtó kiválasztása |
| Enter | SMART lekérése |
| 1 / 2 / 3 | Összefoglaló / teljes jelentés / JSON |
| R / U | Meghajtólista frissítése / USB-adaptertípus váltása |
| Q vagy Esc | Kilépés |

```bash
./target/release/roadmap list
sudo ./target/release/roadmap smart /dev/nvme0n1
sudo ./target/release/roadmap smart /dev/nvme0n1 --json
sudo ./target/release/roadmap smart /dev/sdX --type sat --full
```

Az utolsó példában a `/dev/sdX` helyére a saját meghajtód útvonala kerül. A `--type sat` SAT-átvitelt támogató USB-adapterhez használható. A `smart` érvényes vagy részleges jelentés esetén kiírja az elérhető adatokat, és a `smartctl` státuszkódját adja vissza.

Az összefoglaló az elérhető modell-, firmware- és kapacitásadatokon túl az írt és olvasott adatmennyiséget, az üzemórát, a bekapcsolási ciklusokat és a hibaszámlálókat is mutatja. A hiányzó értékeket „–” jelöli. Ismeretlen mértékegységű ATA írásszámlálónál a nyers érték jelenik meg.

## Tesztek és CI

A tesztek mintajelentésekkel futnak, ezért tesztmeghajtó és rendszergazdai jog nélkül is használhatók.

```bash
cargo test --locked --all-targets
cargo test --locked --all-targets --no-default-features
```

A [GitHub Actions workflow](.github/workflows/ci.yml) push és pull request eseményre van beállítva. A grafikus és a terminálos konfiguráció tesztjeit, valamint a release buildet Rust 1.75.0-val és az aktuális stabil verzióval futtatja.
