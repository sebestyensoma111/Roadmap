import argparse
import subprocess
import sys

from .devices import list_devices, show_devices, show_smart

USB_TYPES = ('sat', 'sat,12', 'sat,16', 'scsi', 'sntjmicron', 'sntasmedia', 'sntrealtek')


def menu():
    while True:
        disks = list_devices()
        print('\nRoadmap — meghajtóinformáció és SMART\n')
        show_devices(disks)
        choice = input('\nMeghajtó száma, r = frissítés, 0 = kilépés: ').strip().lower()
        if choice == '0':
            return 0
        if choice == 'r':
            continue
        if not choice.isdecimal() or len(choice) > 6 or not 1 <= int(choice) <= len(disks):
            print('Válassz egy meghajtót a listából.')
            continue
        view = input('Enter = összefoglaló, t = teljes jelentés, j = JSON: ').strip().lower()
        try:
            show_smart(disks[int(choice) - 1]['path'], view={'t': 'full', 'j': 'json'}.get(view, 'summary'))
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            print(f'Hiba: {error}', file=sys.stderr)
        input('\nEnter = vissza a meghajtólistához')


def main():
    parser = argparse.ArgumentParser(description='Linuxos meghajtóinformáció és SMART-lekérdezés')
    commands = parser.add_subparsers(dest='command')
    commands.add_parser('list', help='meghajtók és partíciók listázása')
    smart = commands.add_parser('smart', help='SMART és eszközinformációk lekérdezése')
    smart.add_argument('device', help='a teljes meghajtó eszközútja, például /dev/nvme0n1')
    smart.add_argument('--type', choices=USB_TYPES, help='USB-adapter illesztési típusa')
    view = smart.add_mutually_exclusive_group()
    view.add_argument('--full', action='store_const', const='full', dest='view',
                      help='teljes szöveges jelentés')
    view.add_argument('--json', action='store_const', const='json', dest='view',
                      help='teljes gépi jelentés')
    args = parser.parse_args()
    try:
        if args.command == 'smart':
            return show_smart(args.device, args.type, args.view)
        if args.command == 'list':
            show_devices()
            return 0
        return menu()
    except EOFError:
        return 0
    except KeyboardInterrupt:
        print('\nMegszakítva.', file=sys.stderr)
        return 130
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f'Hiba: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
