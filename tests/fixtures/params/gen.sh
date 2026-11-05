# Black-box generator for the stream-parameter fixtures (ffmpeg 9.0.1 +
# libx264 / libx265). Reference values: ffprobe -show_streams.
#
# The VP9 / AV1 files (*.mp4) are copies of oxideav-mp4's
# tests/fixtures/params ones: ffmpeg writes vp09 / av01 only into ISO
# (.mp4) files, which the QuickTime demuxer reads as well.
set -e
S="-f lavfi -i testsrc2=size=32x18:rate=25 -frames:v 2 -map_metadata -1 -fflags +bitexact"
for pf in yuv420p yuv420p10le yuv422p yuv444p; do ffmpeg -y -v error $S -c:v libx264 -preset ultrafast -pix_fmt $pf h264-$pf.mov; done
for pf in yuv420p yuv420p10le yuv422p10le yuv444p gray; do ffmpeg -y -v error $S -c:v libx265 -preset ultrafast -x265-params log-level=none -tag:v hvc1 -pix_fmt $pf hevc-$pf.mov; done
ffmpeg -y -v error -f lavfi -i testsrc2=size=32x18:rate=30000/1001 -frames:v 3 -map_metadata -1 -fflags +bitexact -c:v libx264 -preset ultrafast -pix_fmt yuv420p h264-ntsc.mov
A="-f lavfi -i sine=frequency=440:sample_rate=8000:duration=0.02 -map_metadata -1 -fflags +bitexact"
for c in pcm_s16be pcm_s16le pcm_s24be pcm_s32be pcm_f32be pcm_f64be pcm_u8 pcm_s8; do ffmpeg -y -v error $A -c:a $c $c.mov; done
# Audio stream parameters (ffmpeg 9.0.1; HE-AAC from the macOS afconvert
# encoder, remuxed by ffmpeg -c copy). Reference values: ffprobe
# -show_streams, cross-checked with afinfo.
for c in pcm_s24le pcm_s32le pcm_f32le pcm_f64le pcm_mulaw pcm_alaw adpcm_ima_qt; do ffmpeg -y -v error $A -c:a $c $c.mov; done
B="-f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.01 -map_metadata -1 -fflags +bitexact"
ffmpeg -y -v error $B -af aformat=channel_layouts=5.1 -c:a pcm_s16le lpcm-s16le-6ch.mov
ffmpeg -y -v error $B -af aformat=channel_layouts=5.1 -c:a pcm_f32le lpcm-f32le-6ch.mov
ffmpeg -y -v error $B -af aformat=channel_layouts=5.1 -c:a pcm_s24be lpcm-s24be-6ch.mov
ffmpeg -y -v error -f lavfi -i sine=frequency=440:sample_rate=96000:duration=0.01 -ac 2 -map_metadata -1 -fflags +bitexact -c:a pcm_s16le lpcm-s16le-96k.mov
C="-f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.05 -map_metadata -1 -fflags +bitexact"
ffmpeg -y -v error -f lavfi -i sine=frequency=440:sample_rate=44100:duration=0.05 -ac 1 -map_metadata -1 -fflags +bitexact -c:a aac aac-mono.mov
for l in 3.0 4.0 5.0 5.1 7.1; do ffmpeg -y -v error $C -af aformat=channel_layouts=$l -c:a aac aac-$l.mov; done
# Layouts outside Table 1.23: the encoder writes a program_config_element.
ffmpeg -y -v error $C -af aformat=channel_layouts=2.1 -c:a aac aac-pce-2.1.mov
ffmpeg -y -v error $C -af aformat=channel_layouts=6.1 -c:a aac aac-pce-6.1.mov
ffmpeg -y -v error $C -af "aformat=channel_layouts=7.1(wide)" -c:a aac aac-pce-7.1wide.mov
T=$(mktemp -d)
ffmpeg -y -v error -f lavfi -i sine=frequency=440:sample_rate=44100:duration=0.3 -ac 2 -c:a pcm_s16le $T/he-src2.wav
ffmpeg -y -v error -f lavfi -i sine=frequency=440:sample_rate=44100:duration=0.3 -ac 1 -c:a pcm_s16le $T/he-src1.wav
afconvert -f m4af -d aach $T/he-src2.wav $T/he-stereo.m4a
afconvert -f m4af -d aach $T/he-src1.wav $T/he-mono.m4a
afconvert -f m4af -d aacp $T/he-src2.wav $T/he-v2.m4a
for f in he-stereo he-mono he-v2; do ffmpeg -y -v error -i $T/$f.m4a -c copy -map_metadata -1 -fflags +bitexact aac-$f.mov; done
