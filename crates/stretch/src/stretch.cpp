// Musicable's C interface to Signalsmith Stretch, which src/lib.rs binds by
// hand. Audio goes in and out a channel at a time, as the engine keeps it, so
// nothing is copied on the way. Nothing here throws across the interface:
// a stretcher that can't be made comes back null, and the calls that render
// say whether they did.

#include "signalsmith-stretch/signalsmith-stretch.h"

#include <cstdint>

using Stretcher = signalsmith::stretch::SignalsmithStretch<float>;

extern "C" {

struct musicable_stretch {
    Stretcher stretcher;

    // Seeded, so the same calls give the same audio every time, on the same
    // build: the stretcher draws random phases for stretches past 2x.
    explicit musicable_stretch(uint32_t seed) : stretcher(long(seed)) {}
};

musicable_stretch *musicable_stretch_new(int channels, float sample_rate, uint32_t seed) {
    try {
        auto *stretch = new musicable_stretch(seed);
        stretch->stretcher.presetDefault(channels, sample_rate);
        return stretch;
    } catch (...) {
        return nullptr;
    }
}

void musicable_stretch_free(musicable_stretch *stretch) {
    delete stretch;
}

int musicable_stretch_block_samples(const musicable_stretch *stretch) {
    return stretch->stretcher.blockSamples();
}

int musicable_stretch_interval_samples(const musicable_stretch *stretch) {
    return stretch->stretcher.intervalSamples();
}

int musicable_stretch_input_latency(const musicable_stretch *stretch) {
    return stretch->stretcher.inputLatency();
}

int musicable_stretch_output_latency(const musicable_stretch *stretch) {
    return stretch->stretcher.outputLatency();
}

void musicable_stretch_reset(musicable_stretch *stretch) {
    stretch->stretcher.reset();
}

void musicable_stretch_set_transpose(musicable_stretch *stretch, float factor) {
    stretch->stretcher.setTransposeFactor(factor);
}

void musicable_stretch_set_formants(musicable_stretch *stretch, float factor, int compensate_pitch) {
    stretch->stretcher.setFormantFactor(factor, compensate_pitch != 0);
}

// `frequency` is a fraction of the sample rate, as Signalsmith Stretch takes it.
void musicable_stretch_set_formant_base(musicable_stretch *stretch, float frequency) {
    stretch->stretcher.setFormantBase(frequency);
}

int musicable_stretch_seek(musicable_stretch *stretch, const float *const *inputs, int frames,
                           double rate) {
    try {
        stretch->stretcher.seek(inputs, frames, rate);
        return 1;
    } catch (...) {
        return 0;
    }
}

int musicable_stretch_process(musicable_stretch *stretch, const float *const *inputs,
                              int input_frames, float *const *outputs, int output_frames) {
    try {
        stretch->stretcher.process(inputs, input_frames, outputs, output_frames);
        return 1;
    } catch (...) {
        return 0;
    }
}

// A call's input of which only the end is in memory: the frames before
// `skip` read as silence. The stretcher reads all of a call's input to see
// whether it's silent, but keeps only its last block and interval.
struct TailChannel {
    const float *tail;
    int skip;

    float operator[](int i) const {
        return i < skip ? 0.0f : tail[i - skip];
    }
};

struct TailInputs {
    const float *const *tails;
    int skip;

    TailChannel operator[](int c) const {
        return TailChannel{tails[c], skip};
    }
};

int musicable_stretch_process_tail(musicable_stretch *stretch, const float *const *tails,
                                   int tail_frames, int input_frames, float *const *outputs,
                                   int output_frames) {
    try {
        TailInputs inputs{tails, input_frames - tail_frames};
        stretch->stretcher.process(inputs, input_frames, outputs, output_frames);
        return 1;
    } catch (...) {
        return 0;
    }
}

} // extern "C"
