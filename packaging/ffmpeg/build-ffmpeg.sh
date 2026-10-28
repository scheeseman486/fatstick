#!/bin/sh
# Builds the audio-only ffmpeg and ffprobe that Playlist2USB bundles:
# decoders for every supported source format, LAME for MP3 output, and the
# filters the converter, the loudness meter and the player use. No --enable-gpl
# and no --enable-nonfree: an LGPL 2.1+ build, statically linked with LAME
# (LGPL). This script is also the corresponding source offer: it fetches the
# exact sources it builds from.
#
# Usage: packaging/ffmpeg/build-ffmpeg.sh <target> [work dir]
#   x86_64-unknown-linux-gnu   native, on Linux
#   x86_64-pc-windows-gnu      cross, on Linux with mingw-w64 (also serves the
#                              MSVC-built app: the files are copied under both names)
#   aarch64-apple-darwin       on macOS, Apple Silicon (what the app ships for now)
#   x86_64-apple-darwin        on macOS, Intel
#   universal-apple-darwin     on macOS: both, joined with lipo
# Needs: curl, git, make, a C compiler, nasm (and mingw-w64 for Windows).
# Writes app/src-tauri/binaries/{ffmpeg,ffprobe}-<target>[.exe], where Tauri
# picks up sidecars.
set -eu

TARGET=$1
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${2:-$ROOT/target/ffmpeg-build/$TARGET}
FFMPEG_TAG=n9.0.2
LAME_VER=3.100
LAME_SHA256=ddfe36cab873794038ae2c1210557ad34857a4b6bdc515785d1da9e175b1da1e
OUT=$ROOT/app/src-tauri/binaries
JOBS=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)

mkdir -p "$WORK" "$OUT"
cd "$WORK"
if [ ! -f "lame-$LAME_VER.tar.gz" ]; then
  curl -fsSL -o "lame-$LAME_VER.tar.gz" "https://downloads.sourceforge.net/project/lame/lame/$LAME_VER/lame-$LAME_VER.tar.gz"
fi
if command -v sha256sum >/dev/null; then
  echo "$LAME_SHA256  lame-$LAME_VER.tar.gz" | sha256sum -c -
else
  echo "$LAME_SHA256  lame-$LAME_VER.tar.gz" | shasum -a 256 -c -
fi
[ -d ffmpeg ] || git clone --quiet --depth 1 --branch "$FFMPEG_TAG" https://github.com/FFmpeg/FFmpeg.git ffmpeg

# One architecture: LAME into $1/prefix, then ffmpeg into $1/out.
build_one() {
  dir=$1; lame_host=$2; cflags=$3; ff_extra=$4
  mkdir -p "$dir"
  rm -rf "$dir/lame-$LAME_VER"
  tar xzf "lame-$LAME_VER.tar.gz" -C "$dir"
  (cd "$dir/lame-$LAME_VER" &&
    CFLAGS="-O2 $cflags" LDFLAGS="$cflags" ./configure --quiet --prefix="$dir/prefix" $lame_host \
      --enable-static --disable-shared --disable-frontend --disable-gtktest --disable-decoder &&
    make -s -j"$JOBS" && make -s install)
  mkdir -p "$dir/build"
  (cd "$dir/build" &&
    "$WORK/ffmpeg/configure" --prefix="$dir/out" $ff_extra \
      --extra-cflags="-I$dir/prefix/include $cflags" --extra-ldflags="-L$dir/prefix/lib $cflags" \
      --disable-everything --disable-autodetect --disable-doc --disable-network --disable-debug \
      --disable-ffplay --enable-ffprobe --enable-static --disable-shared \
      --enable-libmp3lame \
      --enable-protocol=file,pipe \
      --enable-demuxer=mp3,flac,wav,w64,aiff,mov,ogg,asf,matroska,aac,ape,wv,caf \
      --enable-parser=mpegaudio,flac,aac,vorbis,opus \
      --enable-decoder=mp1,mp2,mp3,mp3float,flac,alac,aac,vorbis,opus,wmav1,wmav2,wmapro,wmalossless,ape,wavpack,pcm_s16le,pcm_s24le,pcm_s32le,pcm_f32le,pcm_f64le,pcm_s16be,pcm_s24be,pcm_s32be,pcm_u8 \
      --enable-encoder=libmp3lame,pcm_s16le,pcm_f32le \
      --enable-muxer=mp3,wav,pcm_f32le,pcm_s16le,null,data \
      --enable-filter=aresample,aformat,volume,ebur128,pan,anull,abuffer,abuffersink,anullsink,atrim &&
    make -s -j"$JOBS" && make -s install)
}

case "$TARGET" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu)
    build_one "$WORK/$TARGET" "" "" ""
    for t in ffmpeg ffprobe; do
      cp "$WORK/$TARGET/out/bin/$t" "$OUT/$t-$TARGET"
      strip "$OUT/$t-$TARGET"
    done
    ;;
  x86_64-pc-windows-gnu|x86_64-pc-windows-msvc)
    build_one "$WORK/win64" "--host=x86_64-w64-mingw32" "" \
      "--target-os=mingw32 --arch=x86_64 --cross-prefix=x86_64-w64-mingw32- --enable-cross-compile --enable-w32threads --extra-libs=-static"
    for t in ffmpeg ffprobe; do
      for triple in x86_64-pc-windows-gnu x86_64-pc-windows-msvc; do
        cp "$WORK/win64/out/bin/$t.exe" "$OUT/$t-$triple.exe"
        x86_64-w64-mingw32-strip "$OUT/$t-$triple.exe"
      done
    done
    ;;
  aarch64-apple-darwin|x86_64-apple-darwin|universal-apple-darwin)
    if [ "$TARGET" != x86_64-apple-darwin ]; then
      build_one "$WORK/arm64" "--host=aarch64-apple-darwin" "-arch arm64" "--arch=arm64 --enable-cross-compile --target-os=darwin --enable-pthreads"
      for t in ffmpeg ffprobe; do cp "$WORK/arm64/out/bin/$t" "$OUT/$t-aarch64-apple-darwin"; done
    fi
    if [ "$TARGET" != aarch64-apple-darwin ]; then
      build_one "$WORK/x86_64" "--host=x86_64-apple-darwin" "-arch x86_64" "--arch=x86_64 --enable-cross-compile --target-os=darwin --enable-pthreads"
      for t in ffmpeg ffprobe; do cp "$WORK/x86_64/out/bin/$t" "$OUT/$t-x86_64-apple-darwin"; done
    fi
    if [ "$TARGET" = universal-apple-darwin ]; then
      for t in ffmpeg ffprobe; do
        lipo -create "$OUT/$t-aarch64-apple-darwin" "$OUT/$t-x86_64-apple-darwin" -output "$OUT/$t-universal-apple-darwin"
      done
    fi
    ;;
  *)
    echo "unknown target $TARGET" >&2
    exit 2
    ;;
esac
ls -l "$OUT"/ffmpeg-* "$OUT"/ffprobe-*
