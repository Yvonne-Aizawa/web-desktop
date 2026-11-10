"""Build the throwaway trees the probes browse, under $E2E_WORK (default /tmp).

Run as `python3 fixtures.py <name>`, which prints the root it built. The name is
one of:

  root      the file browser's playground: text, binary, images, symlinks, a
            hidden file, and a few things that are not what their name says
  trash     a root with a protected `Lumina/` inside it, for the trash and
            read-only tests
  readonly  a small root to be served with writing switched off
  none      nothing; for the probes that only look at the shell or a catalog

Fixtures are rebuilt per scenario rather than shared, because several probes
delete and create files as part of what they are testing.
"""

import os
import shutil
import sys

WORK = os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e"

# Small, deterministic images. Dimensions *and* colours are part of what the
# preview and wallpaper tests assert on, so both are fixed here: the paint test
# reads the dominant colour back out of a screenshot.
IMAGES = [
    ("red.png", "PNG", (240, 160), (255, 0, 0)),
    ("blue.jpg", "JPEG", (240, 160), (0, 0, 255)),
    ("green.gif", "GIF", (240, 160), (0, 128, 0)),
    ("purple.webp", "WEBP", (240, 160), (168, 85, 247)),
    ("orange.bmp", "BMP", (240, 160), (251, 146, 60)),
    ("teal.tiff", "TIFF", (240, 160), (45, 212, 191)),
]

LOGO_SVG = """<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80" viewBox="0 0 120 80">
  <rect width="120" height="80" rx="8" fill="#131a2b"/>
  <circle cx="60" cy="40" r="22" fill="#4ade80"/>
  <text x="60" y="46" font-family="monospace" font-size="14" text-anchor="middle" fill="#0b0d12">svg</text>
</svg>
"""


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as fh:
        fh.write(text)


def images(directory):
    from PIL import Image

    os.makedirs(directory, exist_ok=True)
    for name, fmt, size, colour in IMAGES:
        Image.new("RGB", size, colour).save(os.path.join(directory, name), fmt)
    write(os.path.join(directory, "logo.svg"), LOGO_SVG)
    # Real image formats the browser cannot decode, and files that lie about
    # what they are: both must degrade to a download rather than a broken tile.
    write(os.path.join(directory, "broken.png"), "this is not really a png\njust text with a .png name\n")
    write(os.path.join(directory, "fake.jpg"), "not really a jpeg either")
    # Sparse, so a 70 MiB file costs nothing on disk and is never read whole.
    with open(os.path.join(directory, "huge.bmp"), "wb") as fh:
        fh.truncate(70 * 1024 * 1024)


def root(work):
    """The file browser's playground, mirroring the layout the probes assume."""
    base = os.path.join(work, "root")
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(base)

    write(os.path.join(base, "README.txt"), "A place to keep files.\n")
    write(os.path.join(base, ".hidden-example"), "you only see me with hidden files shown\n")
    # the file browser's own todo list, which the probes read the content of
    write(os.path.join(base, "notes", "todo.md"),
          "# todo\n\n- [x] window manager\n- [ ] write support\n- [ ] everything else\n")
    write(os.path.join(base, "notes", "scratch.rs"), "fn main() {\n    println!(\"scratch\");\n}\n")

    testdir = os.path.join(base, "testdir")
    os.makedirs(testdir)
    write(os.path.join(testdir, "greeting.txt"), "hello\nworld\n")
    # past the preview cap, so the browser has to truncate it
    write(os.path.join(testdir, "big.txt"), "big file\n" * 40000)
    with open(os.path.join(testdir, "blob.bin"), "wb") as fh:
        fh.write(b"PNG\x00\x01\x02\x03binary\x00data")
    # A link out of the root and a link inside it: the fence must refuse the
    # first and follow the second.
    for link, target in (("escape", "/etc"), ("notes-link", "../notes")):
        path = os.path.join(testdir, link)
        if not os.path.lexists(path):
            os.symlink(target, path)

    images(os.path.join(base, "preview-test"))
    return base


def trash(work):
    """A root with something in it that must survive every delete test."""
    base = root(work)
    base = os.path.join(work, "trash")
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(os.path.join(base, "Lumina", "gallery"))
    write(os.path.join(base, "Lumina", "gallery", "precious.png"), "the one file that must survive\n")
    write(os.path.join(base, "Lumina", "notes.txt"), "mine, not the browser's\n")
    write(os.path.join(base, "notes", "todo.md"), "todo\n")
    write(os.path.join(base, "notes", "scratch.rs"), "fn main() {}\n")
    write(os.path.join(base, "scratch.txt"), "scratch contents\n")
    write(os.path.join(base, "README.txt"), "hello\n")
    return base


def readonly(work):
    base = os.path.join(work, "readonly")
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(base)
    # The text is asserted on, so it says what it is.
    write(os.path.join(base, "notes.txt"), "read-only file\n")
    write(os.path.join(base, "prose.txt"), "read-only file\nsecond line\n")
    write(os.path.join(base, "notes", "todo.md"), "todo\n")
    return base


BUILDERS = {"root": root, "trash": trash, "readonly": readonly, "none": lambda work: work}


def build(name):
    work = WORK
    os.makedirs(work, exist_ok=True)
    return BUILDERS[name](work)


if __name__ == "__main__":
    if len(sys.argv) != 2 or sys.argv[1] not in BUILDERS:
        sys.exit(f"usage: fixtures.py [{'|'.join(BUILDERS)}]")
    print(build(sys.argv[1]))
