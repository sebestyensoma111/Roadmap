import json
import os
import stat
import subprocess
from pathlib import Path


def list_devices():
    fields = 'NAME,PATH,TYPE,SIZE,TRAN,VENDOR,MODEL,SERIAL,REV,FSTYPE,MOUNTPOINTS'
    output = subprocess.check_output(['lsblk', '--bytes', '--json', '--output', fields])
    return [item for item in json.loads(output)['blockdevices'] if item['type'] == 'disk']


def printable(value):
    return ''.join(char if char.isprintable() else ' ' for char in str(value))


def show_devices(disks=None):
    if disks is None:
        disks = list_devices()
    if not disks:
        print('Nincs megjeleníthető fizikai meghajtó.')
    for index, disk in enumerate(disks, 1):
        transport = printable(disk['tran'] or 'ismeretlen busz')
        model = printable(disk['model'] or 'ismeretlen modell').strip()
        print(f"{index:2}. {disk['path']:16} {disk['size'] / 2**30:8.2f} GiB  {transport}  {model}")
        for child in disk.get('children', []):
            if child['type'] == 'part':
                fs = printable(child['fstype'] or 'ismeretlen')
                print(f"    {child['path']:16} {child['size'] / 2**30:8.2f} GiB  {fs}")


def read_smart(path, device_type=None):
    path = os.path.realpath(path)
    disks = list_devices()
    disk = next((item for item in disks if item['path'] == path), None)
    info = os.stat(path)
    nvme_controller = Path('/sys/class/nvme', Path(path).name)
    if not path.startswith('/dev/') or not (
        (stat.S_ISBLK(info.st_mode) and disk)
        or (stat.S_ISCHR(info.st_mode) and nvme_controller.is_dir())
    ):
        raise ValueError('Válassz teljes meghajtót vagy NVMe-vezérlőt a /dev alól.')
    command = ['smartctl', '--xall', '--json=ov']
    if device_type:
        command.extend(['-d', device_type])
    try:
        result = subprocess.run(command + [path], capture_output=True, text=True,
                                errors='replace', timeout=30, check=False)
    except FileNotFoundError as error:
        raise ValueError('A SMART-lekérdezéshez telepítsd a smartmontools csomagot.') from error
    try:
        data = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ValueError(result.stderr.strip() or 'A smartctl nem adott JSON-jelentést.') from error
    return path, disk or {}, data, result.returncode


def first(*values):
    return next((value for value in values if value is not None and value != ''), None)


def amount(value):
    if value is None:
        return None
    return f'{value:,} B  ({value / 10**9:.2f} GB / {value / 10**12:.3f} TB)'


def pci_vendor_name(vendor_id):
    if vendor_id is None:
        return None
    prefix = f'{vendor_id:04x}  '
    for path in ('/usr/share/hwdata/pci.ids', '/usr/share/misc/pci.ids'):
        try:
            with open(path, encoding='utf-8', errors='replace') as database:
                return next((line[len(prefix):].strip() for line in database
                             if line.startswith(prefix)), None)
        except OSError:
            continue
    return None


def units_as_bytes(nvme, stats, ata_raw, data, direction):
    key = f'data_units_{direction}'
    count = first(nvme.get(key + '_s'), nvme.get(key))
    if count is not None:
        return int(count) * 512_000
    stat_name = {'read': 'Logical Sectors Read', 'written': 'Logical Sectors Written'}[direction]
    sectors = stats.get(stat_name)
    sector_size = data.get('logical_block_size')
    if sectors is not None and sector_size:
        return sectors * sector_size
    operation = 'Reads' if direction == 'read' else 'Writes'
    for suffix, scale in (('32MiB', 32 * 2**20), ('MiB', 2**20), ('GiB', 2**30)):
        for prefix in ('Host', 'Total_Host'):
            value = ata_raw.get(f'{prefix}_{operation}_{suffix}')
            if isinstance(value, int):
                return value * scale
    return None


def summary(path, disk, data):
    nvme = data.get('nvme_smart_health_information_log', {})
    stats = {
        item['name']: item['value']
        for page in data.get('ata_device_statistics', {}).get('pages', [])
        for item in page.get('table', [])
        if 'value' in item and not item.get('flags', {}).get('normalized')
    }
    ata = {
        item['name']: item['raw'].get('string', item['raw'].get('value'))
        for item in data.get('ata_smart_attributes', {}).get('table', [])
        if 'raw' in item
    }
    ata_raw = {item['name']: item.get('raw', {}).get('value')
               for item in data.get('ata_smart_attributes', {}).get('table', [])}
    scsi_cycle = data.get('scsi_start_stop_cycle_counter', {})
    scsi_errors = data.get('scsi_error_counter_log', {})
    pci = data.get('nvme_pci_vendor', {})
    power_time = first(data.get('power_on_time', {}).get('hours'), nvme.get('power_on_hours'),
                       stats.get('Power-on Hours'))
    temperature = first(data.get('temperature', {}).get('current'), nvme.get('temperature'))
    health = data.get('smart_status', {}).get('passed')
    vendor = first(data.get('vendor'), (disk.get('vendor') or '').strip())
    if vendor == 'ATA':
        vendor = None
    written = units_as_bytes(nvme, stats, ata_raw, data, 'written')
    read = units_as_bytes(nvme, stats, ata_raw, data, 'read')
    rows = [
        ('Eszköz', path),
        ('Gyártó / jelentett márka', first(vendor, pci_vendor_name(pci.get('id')))),
        ('Modellcsalád', data.get('model_family')),
        ('Modell', first(data.get('model_name'), data.get('product'), disk.get('model'))),
        ('Sorozatszám', first(data.get('serial_number'), disk.get('serial'))),
        ('Firmware', first(data.get('firmware_version'), data.get('revision'), disk.get('rev'))),
        ('NVMe PCI gyártóazonosító', hex(pci['id']) if 'id' in pci else None),
        ('Protokoll / busz', first(data.get('device', {}).get('protocol'), disk.get('tran'))),
        ('Kapacitás', amount(first(data.get('user_capacity', {}).get('bytes'), disk.get('size')))),
        ('Logikai / fizikai szektor', ' / '.join(
            f'{value} B' if value is not None else '–'
            for value in (data.get('logical_block_size'), data.get('physical_block_size')))),
        ('Fordulatszám', data.get('rotation_rate')),
        ('SMART állapot', {True: 'megfelelt', False: 'hibát jelez', None: None}[health]),
        ('Hőmérséklet', f'{temperature} °C' if temperature is not None else None),
        ('Üzemidő', f'{power_time} óra' if power_time is not None else None),
        ('Bekapcsolási ciklusok', first(data.get('power_cycle_count'), nvme.get('power_cycles'),
                                        stats.get('Lifetime Power-On Resets'), ata.get('Power_Cycle_Count'))),
        ('Motor indítás/leállítás ciklusok', first(scsi_cycle.get('accumulated_start_stop_cycles'),
                                                   stats.get('Spindle Motor Start/Stop Cycles'),
                                                   ata.get('Start_Stop_Count'))),
        ('Szabálytalan leállások', first(nvme.get('unsafe_shutdowns'),
                                        ata.get('Unsafe_Shutdown_Count'), ata.get('Unexpected_Power_Loss'))),
        ('Vészparkolások (ATA)', first(stats.get('Number of High Priority Unload Events'),
                                       ata.get('Power-Off_Retract_Count'))),
        ('Összes írt adat (host)', amount(written)),
        ('Összes olvasott adat (host)', amount(read)),
        ('Írási parancsok', first(nvme.get('host_writes'), stats.get('Number of Write Commands'))),
        ('Olvasási parancsok', first(nvme.get('host_reads'), stats.get('Number of Read Commands'))),
        ('NVMe elhasználódás', f"{nvme['percentage_used']}%" if 'percentage_used' in nvme else None),
        ('NVMe tartalékkapacitás', f"{nvme['available_spare']}%" if 'available_spare' in nvme else None),
        ('Áthelyezett szektorok', first(stats.get('Number of Reallocated Logical Sectors'),
                                        ata.get('Reallocated_Sector_Ct'), data.get('scsi_grown_defect_list'))),
        ('Függőben lévő szektorok', ata.get('Current_Pending_Sector')),
        ('Javíthatatlan ATA szektorok', ata.get('Offline_Uncorrectable')),
        ('NVMe média- és adatintegritási hibák', nvme.get('media_errors')),
        ('SCSI javíthatatlan olvasási hibák', scsi_errors.get('read', {}).get('total_uncorrected_errors')),
        ('SCSI javíthatatlan írási hibák', scsi_errors.get('write', {}).get('total_uncorrected_errors')),
    ]
    if written is None:
        rows.extend((f'ATA írási attribútum ({name})', value) for name, value in ata.items()
                    if 'writ' in name.lower() or 'lba' in name.lower())
    return rows


def show_smart(path, device_type=None, view='summary'):
    path, disk, data, code = read_smart(path, device_type)
    if view == 'json':
        print(json.dumps(data, ensure_ascii=True, indent=2))
    elif view == 'full':
        report = data.get('smartctl', {}).get('output', [])
        print('\n'.join(map(printable, report)) if report else json.dumps(data, indent=2))
    else:
        for label, value in summary(path, disk, data):
            print(f'{label:39} {printable(value) if value is not None else "–"}')
        if code:
            print(f'\nSMART állapotkód: {code}')
        for message in data.get('smartctl', {}).get('messages', []):
            print(printable(message['string']))
    return code
