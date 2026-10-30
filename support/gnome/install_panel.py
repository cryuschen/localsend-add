# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
"""Install this local CLI and GNOME extension, preserving a narrow rollback."""
from datetime import datetime
from zoneinfo import ZoneInfo
import ast
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[3]
SOURCE = ROOT / 'source'
BINARY = SOURCE / 'target/release/localsend-cli'
PICKER_BINARY = SOURCE / 'target/release/localsend-picker'
HOME_DIR = Path.home()
TARGET = HOME_DIR / '.local/bin/localsend-cli'
PICKER_TARGET = HOME_DIR / '.local/bin/localsend-picker'
EXTENSION = HOME_DIR / '.local/share/gnome-shell/extensions/localsend-transfer@local'
MENU = HOME_DIR / '.local/share/nautilus-python/extensions/localsend_menu.py'
UUID = 'localsend-transfer@local'


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else None


def replace(source, target, mode):
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix='.'+target.name+'.', dir=target.parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(source.read_bytes()); stream.flush(); os.fsync(stream.fileno()); os.fchmod(stream.fileno(),mode)
        os.replace(name,target)
    finally:
        if os.path.exists(name): os.unlink(name)


def settings(key):
    return ast.literal_eval(subprocess.check_output(['gsettings','get','org.gnome.shell',key],text=True))


def set_settings(key, value):
    subprocess.run(['gsettings','set','org.gnome.shell',key,repr(value)],check=True)


extension_only = '--extension-only' in sys.argv[1:]
cli_only = '--cli-only' in sys.argv[1:]
backend_menu = '--backend-menu' in sys.argv[1:]
picker_only = '--picker-only' in sys.argv[1:]
assert sum([extension_only,cli_only,backend_menu,picker_only]) <= 1, 'Choose one installation mode'
updates_extension = not (cli_only or backend_menu)
version = subprocess.check_output([TARGET if extension_only or picker_only else BINARY,'--version'],text=True).strip()
assert version.startswith('localsend-cli 1.18.2-local-unified.'), version
assert TARGET.is_file() and not TARGET.is_symlink()
status = json.loads(subprocess.check_output([TARGET,'status','--json'],text=True))
if not (extension_only or picker_only):
    assert not status.get('sending') and not status.get('receiving') and not status.get('pending_sender') and not status.get('queued'), 'Wait until transfers and incoming requests are idle before installing'
config = HOME_DIR / '.config/localsend-cli'
config_hashes = {p.name:digest(p) for p in [config/'config.toml',config/'identity.pem',config/'paired-v2.json']}
backup = ROOT/'development-setup'/datetime.now(ZoneInfo('Asia/Shanghai')).strftime('panel-backup-%Y%m%d-%H%M%S')
backup.mkdir(mode=0o700)
plan = [] if extension_only or picker_only else [(BINARY,TARGET,0o755)]
if not cli_only:
    if not (extension_only or picker_only): plan.append((SOURCE/'support/nautilus/localsend_menu.py',MENU,0o644))
    if updates_extension:
        assert subprocess.check_output([PICKER_BINARY,'--version'],text=True).strip() == 'localsend-picker 1.18.2'
        plan.append((PICKER_BINARY,PICKER_TARGET,0o755))
        plan += [(path,EXTENSION/path.name,0o644) for path in sorted((Path(__file__).parent/UUID).iterdir())
                 if path.is_file() and (not picker_only or path.name in {'extension.js','metadata.json','send_dialog.py'})]
manifest = {'version':version,'backup':str(backup),'files':[], 'config_sha256':config_hashes,
            'enabled_before':settings('enabled-extensions'),'disabled_before':settings('disabled-extensions'),
            'was_running':status.get('running',False),'requires_relogin':updates_extension,
            'requires_files_reload':not (cli_only or extension_only or picker_only),
            'restart_backend':not (extension_only or picker_only) and status.get('running',False),
            'picker_available_now':updates_extension,
            'extension_version':json.loads((Path(__file__).parent/UUID/'metadata.json').read_text())['version']}
for index,(source,target,mode) in enumerate(plan):
    assert not target.is_symlink(), target
    old=digest(target)
    record={'path':str(target),'original_sha256':old,'backup':str(backup/str(index)),'mode':mode}
    if old: shutil.copy2(target,record['backup'])
    manifest['files'].append(record)
(backup/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2))
# The backend checks idleness atomically; a newly arrived transfer prevents stop.
if manifest['restart_backend']:
    subprocess.run([TARGET,'stop'],check=True,capture_output=True,text=True)
applied=[]
try:
    for (source,target,mode),record in zip(plan,manifest['files']):
        assert digest(target)==record['original_sha256'], 'Concurrent install detected: '+str(target)
        replace(source,target,mode);applied.append(record)
        record['installed_sha256']=digest(target)
    if updates_extension:
        enabled=settings('enabled-extensions')
        if UUID not in enabled: set_settings('enabled-extensions',enabled+[UUID])
        disabled=settings('disabled-extensions')
        if UUID in disabled: set_settings('disabled-extensions',[item for item in disabled if item!=UUID])
    if manifest['restart_backend']:
        manifest['status_after']=json.loads(subprocess.check_output([TARGET,'start','--json'],text=True))
    assert {name:digest(config/name) for name in config_hashes}==config_hashes, 'Configuration changed during install'
except Exception:
    for record in reversed(applied):
        target=Path(record['path'])
        if record['original_sha256']: replace(Path(record['backup']),target,Path(record['backup']).stat().st_mode&0o777)
        else: target.unlink(missing_ok=True)
    if updates_extension:
        set_settings('enabled-extensions',manifest['enabled_before'])
        set_settings('disabled-extensions',manifest['disabled_before'])
    if manifest['restart_backend']: subprocess.run([TARGET,'start'],check=False,capture_output=True)
    raise
(backup/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2))
(ROOT/'development-setup/panel-install.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2))
print(json.dumps({'version':version,'backup':str(backup),'files':len(plan),'requires_relogin':updates_extension,
                  'configuration_preserved':True},ensure_ascii=False))
