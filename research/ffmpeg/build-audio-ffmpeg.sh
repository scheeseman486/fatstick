#!/bin/sh
# Builds the audio-only ffmpeg that Fatstick bundles: decoders for every
# supported source format, LAME for MP3 output, and the filters the converter
# and player use. Without --enable-gpl this is an LGPL 2.1+ build.
#
# Usage: build-audio-ffmpeg.sh <work dir> [dir holding libmp3lame.a]
# Needs: git, a C compiler, make, nasm, and a static libmp3lame.a
# (Ubuntu: apt install build-essential nasm libmp3lame-dev).
set -eu
WORK=$(realpath "$1")
LAME_LIB=${2:-/usr/lib/x86_64-linux-gnu}
TAG=${FFMPEG_TAG:-n9.0.2}
mkdir -p "$WORK/lame" "$WORK/build"
# Only the static library goes in this directory, so LAME is linked in.
cp "$LAME_LIB/libmp3lame.a" "$WORK/lame/"
[ -d "$WORK/src" ] || git clone --depth 1 --branch "$TAG" https://github.com/FFmpeg/FFmpeg.git "$WORK/src"
cd "$WORK/build"
"$WORK/src/configure" --prefix="$WORK/out" \
  --disable-everything --disable-autodetect --disable-doc --disable-network --disable-debug \
  --disable-ffplay --enable-ffprobe --enable-static --disable-shared \
  --enable-libmp3lame --extra-ldflags="-L$WORK/lame" \
  --enable-protocol=file,pipe \
  --enable-demuxer=mp3,flac,wav,w64,aiff,mov,ogg,asf,matroska,aac,ape,wv,caf \
  --enable-parser=mpegaudio,flac,aac,vorbis,opus \
  --enable-decoder=mp1,mp2,mp3,mp3float,flac,alac,aac,vorbis,opus,wmav1,wmav2,wmapro,wmalossless,ape,wavpack,pcm_s16le,pcm_s24le,pcm_s32le,pcm_f32le,pcm_f64le,pcm_s16be,pcm_s24be,pcm_s32be,pcm_u8 \
  --enable-encoder=libmp3lame,pcm_s16le,pcm_f32le \
  --enable-muxer=mp3,wav,pcm_f32le,pcm_s16le,null \
  --enable-filter=aresample,aformat,volume,ebur128,pan,anull,abuffer,abuffersink,anullsink,atrim
make -j"$(nproc)"
make install
echo "built: $WORK/out/bin/ffmpeg"
