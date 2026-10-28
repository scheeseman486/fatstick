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

struct fatstick_stretch {
    signalsmith::stretch::SignalsmithStretch<float> s;
    int channels;
};

extern "C" {

fatstick_stretch *fatstick_stretch_new(int channels, float sample_rate) {
    fatstick_stretch *h = new fatstick_stretch;
    h->channels = channels;
    h->s.presetDefault(channels, sample_rate);
    return h;
}

void fatstick_stretch_free(fatstick_stretch *h) { delete h; }

void fatstick_stretch_reset(fatstick_stretch *h) { h->s.reset(); }

int fatstick_stretch_input_latency(fatstick_stretch *h) { return h->s.inputLatency(); }

int fatstick_stretch_output_latency(fatstick_stretch *h) { return h->s.outputLatency(); }

void fatstick_stretch_set(fatstick_stretch *h, float semitones, int keep_formants) {
    h->s.setTransposeSemitones(semitones, 0);
    h->s.setFormantSemitones(0, keep_formants != 0);
}

void fatstick_stretch_process(fatstick_stretch *h, float *input, int input_frames, float *output, int output_frames) {
    Interleaved in(input, h->channels);
    Interleaved out(output, h->channels);
    h->s.process(in, input_frames, out, output_frames);
}

void fatstick_stretch_seek(fatstick_stretch *h, float *input, int input_frames, double rate) {
    Interleaved in(input, h->channels);
    h->s.seek(in, input_frames, rate);
}

void fatstick_stretch_flush(fatstick_stretch *h, float *output, int output_frames) {
    Interleaved out(output, h->channels);
    h->s.flush(out, output_frames);
}

int fatstick_stretch_exact(fatstick_stretch *h, float *input, int input_frames, float *output, int output_frames) {
    Interleaved in(input, h->channels);
    Interleaved out(output, h->channels);
    return h->s.exact(in, input_frames, out, output_frames) ? 1 : 0;
}

} // extern "C"
