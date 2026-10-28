// A small C interface to Signalsmith Stretch, for src/lib.rs. Adapted from
// the signalsmith-stretch crate's wrapper (MIT, Colin Marc). Buffers are
// interleaved; lengths are in frames.

#include <signalsmith-stretch.h>
#include <stddef.h>

namespace {
class Interleaved {
    float *data;
    int channels;

public:
    Interleaved(float *data, int channels) : data(data), channels(channels) {}

    class Channel {
        float *data;
        int channel;
        int stride;

    public:
        Channel(float *data, int channel, int stride) : data(data), channel(channel), stride(stride) {}
        float &operator[](size_t i) { return data[i * stride + channel]; }
        const float &operator[](size_t i) const { return data[i * stride + channel]; }
    };

    Channel operator[](size_t c) { return Channel(data, int(c), channels); }
};
} // namespace

struct p2u_stretch {
    signalsmith::stretch::SignalsmithStretch<float> s;
    int channels;
};

extern "C" {

p2u_stretch *p2u_stretch_new(int channels, float sample_rate) {
    p2u_stretch *h = new p2u_stretch;
    h->channels = channels;
    h->s.presetDefault(channels, sample_rate);
    return h;
}

void p2u_stretch_free(p2u_stretch *h) { delete h; }

void p2u_stretch_reset(p2u_stretch *h) { h->s.reset(); }

int p2u_stretch_input_latency(p2u_stretch *h) { return h->s.inputLatency(); }

int p2u_stretch_output_latency(p2u_stretch *h) { return h->s.outputLatency(); }

void p2u_stretch_set(p2u_stretch *h, float semitones, int keep_formants) {
    h->s.setTransposeSemitones(semitones, 0);
    h->s.setFormantSemitones(0, keep_formants != 0);
}

void p2u_stretch_process(p2u_stretch *h, float *input, int input_frames, float *output, int output_frames) {
    Interleaved in(input, h->channels);
    Interleaved out(output, h->channels);
    h->s.process(in, input_frames, out, output_frames);
}

void p2u_stretch_seek(p2u_stretch *h, float *input, int input_frames, double rate) {
    Interleaved in(input, h->channels);
    h->s.seek(in, input_frames, rate);
}

void p2u_stretch_flush(p2u_stretch *h, float *output, int output_frames) {
    Interleaved out(output, h->channels);
    h->s.flush(out, output_frames);
}

int p2u_stretch_exact(p2u_stretch *h, float *input, int input_frames, float *output, int output_frames) {
    Interleaved in(input, h->channels);
    Interleaved out(output, h->channels);
    return h->s.exact(in, input_frames, out, output_frames) ? 1 : 0;
}

} // extern "C"
