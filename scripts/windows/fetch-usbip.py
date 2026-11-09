"""Fetches the pinned, Microsoft-signed usbip-win2 release and extracts the
files Relay redistributes (BSD-2-Clause) without running its installer.

Usage: python scripts/windows/fetch-usbip.py [out_dir]   (default: vendor/usbip)
Requires: pip install binary-refinery
"""
import hashlib
import sys
import urllib.request
from pathlib import Path

URL = "https://github.com/vadimgrn/usbip-win2/releases/download/v.0.9.8.0/USBip-0.9.8.0-x64.exe"
SHA256 = "81F426741F7EE2ED991FEBE24A22DACA8400B6AE2F171054E3FB404897E15D39"
WANTED = {
    "usbip.exe", "libusbip.dll", "resources.dll", "devnode.exe",
    "usbip2_ude.inf", "usbip2_ude.cat", "usbip2_ude.sys",
    "usbip2_filter.inf", "usbip2_filter.cat", "usbip2_filter.sys",
}
LICENSE_URL = "https://raw.githubusercontent.com/vadimgrn/usbip-win2/v.0.9.8.0/LICENSE.txt"

root = Path(__file__).resolve().parents[2]
out = Path(sys.argv[1]) if len(sys.argv) > 1 else root / "vendor" / "usbip"
out.mkdir(parents=True, exist_ok=True)
cache = root / "target" / "usbip-cache"
cache.mkdir(parents=True, exist_ok=True)
installer = cache / "USBip-0.9.8.0-x64.exe"
if not installer.exists() or hashlib.sha256(installer.read_bytes()).hexdigest().upper() != SHA256:
    print("downloading", URL)
    urllib.request.urlretrieve(URL, installer)
data = installer.read_bytes()
digest = hashlib.sha256(data).hexdigest().upper()
assert digest == SHA256, f"installer hash mismatch: {digest}"

from refinery.lib.inno.archive import InnoArchive  # noqa: E402

archive = InnoArchive(bytearray(data))
found = set()
for f in archive.files:
    name = str(f.path).replace("\\", "/").split("/")[-1]
    if name not in WANTED or f.dupe:
        continue
    content = bytes(archive.check_file(f, archive.extractor(f)()))
    target = out / name
    if name in found:
        assert target.read_bytes() == content, f"ambiguous file {name}"
    target.write_bytes(content)
    found.add(name)
missing = WANTED - found
assert not missing, f"missing from installer: {missing}"
urllib.request.urlretrieve(LICENSE_URL, out / "LICENSE-usbip-win2.txt")
for name in sorted(found):
    print(f"{hashlib.sha256((out / name).read_bytes()).hexdigest().upper()}  {name}")
