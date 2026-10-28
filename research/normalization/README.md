# Lossless MP3 gain: mp3rgain vs mp3gain

`norm_test.py` builds a 60-second synthetic stereo track (tones plus pink noise)
and five variants, encodes them with LAME 3.100 (CBR 320, CBR 256, VBR -V 0, a
loud master, a quiet master, MPEG-2 mono 22.05 kHz 64 kbps), then:

1. applies +2 steps (+3.0 dB) with mp3rgain and with the original mp3gain 1.6.2,
   compares the decoded audio of the two, checks the amplitude ratio is exactly
   2^(2/4) = sqrt(2), and checks that undo restores the file;
2. normalizes to -18 LUFS (ReplayGain 2.0, track mode, clipping prevention) with
   mp3rgain and measures the result with ffmpeg's EBU R128 filter;
3. for comparison, applies the exact gain to the lossless source before a LAME
   256 kbps CBR encode.

```sh
sudo apt install ffmpeg lame mp3gain
cargo install mp3rgain            # 3.14.0 was used for results.json
mkdir work && python3 norm_test.py work "$(which mp3rgain)" > results.json
```

Results (results.json, 2026-10-28): both tools gave identical decoded audio on
all six files, +3.0 dB (+3.1 on MPEG-2 mono) and an amplitude ratio of 1.41421;
normalized files landed within 0.7 dB of -18 LUFS (1.5 dB steps); the re-encode
path landed at -18.0 LUFS. mp3rgain's undo restored the file byte for byte except
one bit in the last frame of the VBR file (decoded audio identical). The design
doc is "Playlist2USB design: playlists, sync, conversion and playback".
