// Musicable's C interface to Rubber Band Library's R3 engine, which
// src/rubber_band.rs binds by hand. It builds the library's own single-file
// source in here, so the engine and this see the same configuration, and
// drives R3Stretcher, the engine itself, since that's where Musicable's
// changes to the library are (see vendor/README.md). Nothing throws across the
// interface: a stretcher that can't be made comes back null, and the calls
// that can fail say whether they did.

#include "../vendor/rubberband/single/RubberBandSingle.cpp"

#include <cstddef>
#include <map>

using RubberBand::Log;
using RubberBand::R3Stretcher;
using RubberBand::RubberBandStretcher;

namespace {

// The library logs its warnings through this: Musicable checks what it
// gets back instead.
Log quiet() {
    return Log([](const char *) {}, [](const char *, double) {},
               [](const char *, double, double) {});
}

RubberBandStretcher::Options options(int real_time) {
    // Stereo as a pair, mid and side, which keeps mixes mono-compatible.
    RubberBandStretcher::Options options =
        RubberBandStretcher::OptionEngineFiner | RubberBandStretcher::OptionChannelsTogether;
    if (real_time) {
        // The pitch may move as it plays.
        options |= RubberBandStretcher::OptionProcessRealTime |
                   RubberBandStretcher::OptionPitchHighConsistency;
    }
    return options;
}

} // namespace

extern "C" {

struct musicable_rubber_band {
    R3Stretcher stretcher;

    musicable_rubber_band(int channels, double sample_rate, int real_time, double ratio,
                          double pitch)
        : stretcher(R3Stretcher::Parameters(sample_rate, channels, options(real_time)), ratio,
                    pitch, quiet()) {}
};

musicable_rubber_band *musicable_rubber_band_new(int channels, double sample_rate, int real_time,
                                                 double ratio, double pitch) {
    try {
        return new musicable_rubber_band(channels, sample_rate, real_time, ratio, pitch);
    } catch (...) {
        return nullptr;
    }
}

void musicable_rubber_band_free(musicable_rubber_band *stretch) {
    delete stretch;
}

void musicable_rubber_band_set_pitch_scale(musicable_rubber_band *stretch, double scale) {
    stretch->stretcher.setPitchScale(scale);
}

// With `preserved`, the formants move by `scale` before the pitch moves
// them, so 1 / the pitch scale keeps them in place; without, the pitch
// moves them, and the stretcher spends nothing on them.
void musicable_rubber_band_set_formants(musicable_rubber_band *stretch, int preserved,
                                        double scale) {
    stretch->stretcher.setFormantOption(preserved ? RubberBandStretcher::OptionFormantPreserved
                                                  : RubberBandStretcher::OptionFormantShifted);
    stretch->stretcher.setFormantScale(scale);
}

void musicable_rubber_band_set_envelope(musicable_rubber_band *stretch, double scale) {
    stretch->stretcher.setFormantEnvelopeScale(scale);
}

int musicable_rubber_band_set_key_frames(musicable_rubber_band *stretch, const size_t *inputs,
                                         const size_t *outputs, size_t count) {
    try {
        std::map<size_t, size_t> keys;
        for (size_t i = 0; i < count; ++i) {
            keys[inputs[i]] = outputs[i];
        }
        stretch->stretcher.setKeyFrameMap(keys);
        return 1;
    } catch (...) {
        return 0;
    }
}

// Offline, it only counts the input, so it takes none.
void musicable_rubber_band_study(musicable_rubber_band *stretch, size_t frames) {
    stretch->stretcher.study(nullptr, frames, true);
}

size_t musicable_rubber_band_start_pad(const musicable_rubber_band *stretch) {
    return stretch->stretcher.getPreferredStartPad();
}

size_t musicable_rubber_band_start_delay(const musicable_rubber_band *stretch) {
    return stretch->stretcher.getStartDelay();
}

size_t musicable_rubber_band_samples_required(const musicable_rubber_band *stretch) {
    return stretch->stretcher.getSamplesRequired();
}

int musicable_rubber_band_process(musicable_rubber_band *stretch, const float *const *inputs,
                                  size_t frames, int last) {
    try {
        stretch->stretcher.process(inputs, frames, last != 0);
        return 1;
    } catch (...) {
        return 0;
    }
}

int musicable_rubber_band_available(const musicable_rubber_band *stretch) {
    return stretch->stretcher.available();
}

size_t musicable_rubber_band_retrieve(const musicable_rubber_band *stretch,
                                      float *const *outputs, size_t frames) {
    return stretch->stretcher.retrieve(outputs, frames);
}

} // extern "C"
