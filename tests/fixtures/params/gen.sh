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
