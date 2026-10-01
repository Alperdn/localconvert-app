#!/usr/bin/env bash
# BUILD-TIME ONLY (MSYS2 UCRT64 shell). Builds the minimal, LGPL-only
# FFmpeg/ffprobe used by MEB-Donusturucu's speech preprocessing.
# Never runs on end-user machines. Invoked by scripts/build-ffmpeg-engine.ps1.
#
# Fails closed: any hash / signature mismatch aborts before extraction.
set -euo pipefail

FFMPEG_VERSION="9.0.2"
FFMPEG_TARBALL="ffmpeg-${FFMPEG_VERSION}.tar.xz"
FFMPEG_URL="https://ffmpeg.org/releases/${FFMPEG_TARBALL}"
FFMPEG_ASC_URL="${FFMPEG_URL}.asc"
FFMPEG_SHA256="8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e"
# FFmpeg release signing key (https://ffmpeg.org/ffmpeg-devel.asc)
FFMPEG_KEY_FPR="FCF986EA15E6E293A5644F10B4322F04D67658D8"
FFMPEG_KEY_URL="https://ffmpeg.org/ffmpeg-devel.asc"

WORK="${1:?usage: build-ffmpeg.sh <work-dir>}"
mkdir -p "$WORK"; cd "$WORK"

fetch() { [ -s "$2" ] || curl -fsSL --max-time 600 -o "$2" "$1"; }
fetch "$FFMPEG_URL" "$FFMPEG_TARBALL"
fetch "$FFMPEG_ASC_URL" "$FFMPEG_TARBALL.asc"
fetch "$FFMPEG_KEY_URL" ffmpeg-devel.asc

got="$(sha256sum "$FFMPEG_TARBALL" | cut -d' ' -f1)"
[ "$got" = "$FFMPEG_SHA256" ] || { echo "SOURCE SHA-256 MISMATCH: $got"; exit 10; }

export GNUPGHOME="$(mktemp -d /tmp/ffmpeg-gnupg.XXXXXX)"; chmod 700 "$GNUPGHOME"
gpg --batch --import ffmpeg-devel.asc >/dev/null 2>&1
fpr="$(gpg --batch --with-colons --fingerprint | awk -F: '/^fpr/{print $10; exit}')"
[ "$fpr" = "$FFMPEG_KEY_FPR" ] || { echo "SIGNING KEY FINGERPRINT MISMATCH: $fpr"; exit 11; }
gpg --batch --status-fd 1 --verify "$FFMPEG_TARBALL.asc" "$FFMPEG_TARBALL" 2>/dev/null \
  | grep -q "^\[GNUPG:\] VALIDSIG $FFMPEG_KEY_FPR" || { echo "GPG SIGNATURE INVALID"; exit 12; }

rm -rf "src" "out"; mkdir src out
tar -xJf "$FFMPEG_TARBALL" -C src --strip-components=1
mkdir build; cd build

# ---- Minimal, LGPL-only, local-files-only configuration -------------------
# Deliberately NO --enable-gpl / --enable-nonfree / --enable-version3 /
# external libraries. --disable-autodetect prevents picking up any library
# that happens to be installed in the build environment.
CONFIGURE_FLAGS=(
  --arch=x86_64 --target-os=mingw32
  --prefix="$WORK/out"
  --disable-everything --disable-autodetect
  --disable-network --disable-doc --disable-debug
  --disable-ffplay --enable-ffmpeg --enable-ffprobe
  --disable-avdevice
  --enable-static --disable-shared
  --extra-ldflags=-static
  --enable-protocol=file
  --enable-demuxer=wav,mp3,aac,mov,flac,ogg
  --enable-parser=aac,aac_latm,mpegaudio,flac,vorbis,opus
  --enable-decoder=pcm_s16le,pcm_s16be,pcm_s24le,pcm_s32le,pcm_u8,pcm_f32le,pcm_f64le,mp3float,mp3,aac,aac_latm,alac,flac,vorbis,opus
  --enable-muxer=wav
  --enable-encoder=pcm_s16le
  --enable-filter=aresample,aformat,anull,anullsink,abuffer,abuffersink
  --enable-bsf=null
)
../src/configure "${CONFIGURE_FLAGS[@]}"
make -j"$(nproc)"
make install

strip out/bin/ffmpeg.exe out/bin/ffprobe.exe 2>/dev/null || strip "$WORK/out/bin/"*.exe

# ---- Build record ---------------------------------------------------------
OUT="$WORK/out"
"$OUT/bin/ffmpeg.exe" -hide_banner -version > "$OUT/ffmpeg-version.txt"
"$OUT/bin/ffmpeg.exe" -hide_banner -buildconf > "$OUT/ffmpeg-buildconf.txt"
"$OUT/bin/ffprobe.exe" -hide_banner -version > "$OUT/ffprobe-version.txt"
cp "$WORK/src/COPYING.LGPLv2.1" "$WORK/src/COPYING.LGPLv3" "$WORK/src/LICENSE.md" "$OUT/" 2>/dev/null || true
{
  echo "compiler=$(gcc --version | head -1)"
  echo "nasm=$(nasm --version)"
  echo "make=$(make --version | head -1)"
  echo "msys2_runtime=$(pacman -Q msys2-runtime)"
  echo "build_date=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "configure_flags=${CONFIGURE_FLAGS[*]}"
  echo "ffmpeg_sha256=$(sha256sum "$OUT/bin/ffmpeg.exe" | cut -d' ' -f1)"
  echo "ffprobe_sha256=$(sha256sum "$OUT/bin/ffprobe.exe" | cut -d' ' -f1)"
} > "$OUT/build-record.txt"
echo "BUILD OK"
