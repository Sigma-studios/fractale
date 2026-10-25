#!/bin/sh
# Regenerates the fixtures with GIMP 2.10 inside the Lima VM. From the
# repository root, copy the tools in and the results out:
#   limactl copy -r scripts/xcf/. repertoire:/tmp/xcftools/
#   limactl shell --workdir / repertoire -- sh /tmp/xcftools/gen.sh
#   limactl copy -r repertoire:/tmp/fx/. tests/fixtures/xcf/
set -e
here=$(cd "$(dirname "$0")" && pwd)
rm -rf /tmp/fx && mkdir -p /tmp/fx
python3 "$here/mkseed.py" /tmp/fx
cd /tmp
gimp -i -d -f -b "(begin (define outdir \"/tmp/fx/\") (load \"$here/gen.scm\"))" -b '(gimp-quit 0)'
rm /tmp/fx/seed_zlib.xcf
ls -l /tmp/fx
