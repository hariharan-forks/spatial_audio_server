//! The audio render function implementation.
//!
//! The render function is passed to `nannou::App`'s build output stream method and describes how
//! audio should be rendered to the output.

use audio::{DISTANCE_BLUR, PROXIMITY_LIMIT_2, Sound, Speaker, MAX_CHANNELS};
use audio::detector::{EnvDetector, Fft, FftDetector, FFT_WINDOW_LEN};
use audio::{dbap, source, sound, speaker};
use audio::fft;
use fxhash::{FxHashMap, FxHashSet};
use gui;
use installation::{self, Installation};
use metres::Metres;
use nannou;
use nannou::audio::Buffer;
use nannou::math::{MetricSpace, Point2};
use osc;
use rustfft::num_complex::Complex;
use rustfft::num_traits::Zero;
use soundscape;
use std;
use std::ops::Deref;
use std::sync::mpsc;
use time_calc::Samples;
use utils;

/// Simplified type alias for the nannou audio output stream used by the audio server.
pub type Stream = nannou::audio::Stream<Model>;

/// A sound that is currently active on the audio thread.
pub struct ActiveSound {
    sound: Sound,
    channel_detectors: Box<[EnvDetector]>,
    total_duration_frames: Option<Samples>,
}

pub struct ActiveSpeaker {
    speaker: Speaker,
    env_detector: EnvDetector,
    fft_detector: FftDetector,
}

impl ActiveSound {
    /// Create a new `ActiveSound`.
    pub fn new(sound: Sound) -> Self {
        let channel_detectors = (0..sound.channels)
            .map(|_| EnvDetector::new())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let total_duration_frames = sound.signal.remaining_frames();
        ActiveSound {
            sound,
            channel_detectors,
            total_duration_frames,
        }
    }

    /// The normalised progress through playback.
    pub fn normalised_progress(&self) -> Option<f64> {
        let remaining_duration = self.signal.remaining_frames();
        let total_duration = self.total_duration_frames;
        let normalised_progress = match (remaining_duration, total_duration) {
            (Some(Samples(remaining)), Some(Samples(total))) => {
                let current_frame = total - remaining;
                Some(current_frame as f64 / total as f64)
            },
            _ => None,
        };
        normalised_progress
    }
}

impl From<Sound> for ActiveSound {
    fn from(sound: Sound) -> Self {
        ActiveSound::new(sound)
    }
}

impl Deref for ActiveSound {
    type Target = Sound;
    fn deref(&self) -> &Self::Target {
        &self.sound
    }
}

struct SpeakerAnalysis {
    rms: f32,
    peak: f32,
    index: usize,
}

/// State that lives on the audio thread.
pub struct Model {
    /// The total number of frames written since the model was created.
    ///
    /// This is used for synchronising `Continuous` WAVs to the audio timeline with sample-perfect
    /// accuracy.
    pub frame_count: u64,
    /// The master volume, controlled via the GUI applied at the very end of processing.
    pub master_volume: f32,
    /// The DBAP rolloff decibel amount, used to attenuate speaker gains over distances.
    pub dbap_rolloff_db: f64,
    /// The set of sources that are currently soloed. If not empty, only these sounds should play.
    pub soloed: FxHashSet<source::Id>,
    /// A map from audio sound IDs to the audio sounds themselves.
    sounds: FxHashMap<sound::Id, ActiveSound>,
    /// A map from speaker IDs to the speakers themselves.
    speakers: FxHashMap<speaker::Id, ActiveSpeaker>,
    // /// A map from a speaker's assigned channel to the ID of the speaker.
    // channel_to_speaker: FxHashMap<usize, speaker::Id>,
    /// A buffer for collecting the speakers within proximity of the sound's position.
    unmixed_samples: Vec<f32>,
    /// A buffer for collecting sounds that have been removed due to completing.
    exhausted_sounds: Vec<sound::Id>,
    /// Channel for communicating active sound info to the GUI.
    gui_audio_monitor_msg_tx: mpsc::SyncSender<gui::AudioMonitorMessage>,
    /// Channel for sending sound analysis data to the OSC output thread.
    osc_output_msg_tx: mpsc::Sender<osc::output::Message>,
    /// A handle to the soundscape thread - for notifying when a sound is complete.
    soundscape_tx: mpsc::Sender<soundscape::Message>,
    /// An analysis per installation to re-use for sending to the OSC output thread.
    installation_analyses: FxHashMap<Installation, Vec<SpeakerAnalysis>>,
    /// A buffer to re-use for DBAP speaker calculations.
    ///
    /// The index of the speaker is its channel.
    dbap_speakers: Vec<dbap::Speaker>,
    /// A buffer to re-use for storing the gain for each speaker produced by DBAP.
    dbap_speaker_gains: Vec<f32>,
    /// The FFT planner used to prepare the FFT calculations and share data between them.
    fft_planner: fft::Planner,
    /// The FFT to re-use by each of the `Detector`s.
    fft: Fft,
    /// A buffer for retrieving the frequency amplitudes from the `fft`.
    fft_frequency_amplitudes_2: Box<[f32; FFT_WINDOW_LEN / 2]>,
}

/// An iterator yielding all `Sound`s in the model.
pub struct SoundsMut<'a> {
    iter: std::collections::hash_map::IterMut<'a, sound::Id, ActiveSound>,
}

impl<'a> Iterator for SoundsMut<'a> {
    type Item = (&'a sound::Id, &'a mut Sound);
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|(id, active)| (id, &mut active.sound))
    }
}

impl Model {
    /// Initialise the `Model`.
    pub fn new(
        gui_audio_monitor_msg_tx: mpsc::SyncSender<gui::AudioMonitorMessage>,
        osc_output_msg_tx: mpsc::Sender<osc::output::Message>,
        soundscape_tx: mpsc::Sender<soundscape::Message>,
    ) -> Self {
        // The currently soloed sources (none by default).
        let soloed = Default::default();

        // A map from audio sound IDs to the audio sounds themselves.
        let sounds = Default::default();

        // A map from speaker IDs to the speakers themselves.
        let speakers = Default::default();

        // A buffer for collecting frames from `Sound`s that have not yet been mixed and written.
        let unmixed_samples = vec![0.0; 1024];

        // A buffer for collecting exhausted `Sound`s.
        let exhausted_sounds = Vec::with_capacity(128);

        // A map from installations to audio analysis frames that can be re-used.
        let installation_analyses = installation::ALL
            .iter()
            .map(|&inst| (inst, Vec::with_capacity(MAX_CHANNELS)))
            .collect();

        // A buffer to re-use for DBAP speaker calculations.
        let dbap_speakers = Vec::with_capacity(MAX_CHANNELS);

        // A buffer to re-use for storing gains produced by DBAP.
        let dbap_speaker_gains = Vec::with_capacity(MAX_CHANNELS);

        // The FFT to re-use by each of the `Detector`s.
        let in_window = [Complex::<f32>::zero(); FFT_WINDOW_LEN];
        let out_window = [Complex::<f32>::zero(); FFT_WINDOW_LEN];
        let fft = Fft::new(in_window, out_window);
        let inverse = false;
        let fft_planner = fft::Planner::new(inverse);

        // A buffer for retrieving the frequency amplitudes from the `fft`.
        let fft_frequency_amplitudes_2 = Box::new([0.0; FFT_WINDOW_LEN / 2]);

        // Initialise the master volume to the default value.
        let master_volume = super::DEFAULT_MASTER_VOLUME;

        // Initialise the rolloff to the default value.
        let dbap_rolloff_db = super::DEFAULT_DBAP_ROLLOFF_DB;

        // Initialise the frame count.
        let frame_count = 0;

        Model {
            frame_count,
            master_volume,
            dbap_rolloff_db,
            soloed,
            sounds,
            speakers,
            unmixed_samples,
            exhausted_sounds,
            installation_analyses,
            gui_audio_monitor_msg_tx,
            osc_output_msg_tx,
            soundscape_tx,
            dbap_speakers,
            dbap_speaker_gains,
            fft,
            fft_planner,
            fft_frequency_amplitudes_2,
        }
    }

    /// Inserts the speaker and sends an `Add` message to the GUI.
    pub fn insert_speaker(&mut self, id: speaker::Id, speaker: Speaker) -> Option<Speaker> {
        // Re-use the old detectors if there are any.
        let (env_detector, fft_detector, old_speaker) = match self.speakers.remove(&id) {
            None => (EnvDetector::new(), FftDetector::new(), None),
            Some(ActiveSpeaker {
                speaker,
                env_detector,
                fft_detector,
            }) => (env_detector, fft_detector, Some(speaker)),
        };

        let speaker = ActiveSpeaker {
            speaker,
            env_detector,
            fft_detector,
        };
        let speaker_msg = gui::SpeakerMessage::Add;
        let msg = gui::AudioMonitorMessage::Speaker(id, speaker_msg);
        self.gui_audio_monitor_msg_tx.try_send(msg).ok();
        self.speakers.insert(id, speaker);
        old_speaker
    }

    /// Removes the speaker and sens a `Removed` message to the GUI.
    pub fn remove_speaker(&mut self, id: speaker::Id) -> Option<Speaker> {
        let removed = self.speakers.remove(&id);
        if removed.is_some() {
            let speaker_msg = gui::SpeakerMessage::Remove;
            let msg = gui::AudioMonitorMessage::Speaker(id, speaker_msg);
            self.gui_audio_monitor_msg_tx.try_send(msg).ok();
        }
        removed.map(|ActiveSpeaker { speaker, .. }| speaker)
    }

    /// Inserts the installation into the speaker with the given `speaker::Id`.
    pub fn insert_speaker_installation(&mut self, id: speaker::Id, inst: Installation) -> bool {
        self.speakers
            .get_mut(&id)
            .map(|active| active.speaker.installations.insert(inst))
            .unwrap_or(false)
    }

    /// Removes the installation from the speaker with the given `speaker::Id`.
    pub fn remove_speaker_installation(&mut self, id: speaker::Id, inst: &Installation) -> bool {
        self.speakers
            .get_mut(&id)
            .map(|active| active.speaker.installations.remove(inst))
            .unwrap_or(false)
    }

    /// Inserts the sound and sends an `Start` active sound message to the GUI.
    pub fn insert_sound(&mut self, id: sound::Id, sound: ActiveSound) -> Option<ActiveSound> {
        let position = sound.position;
        let channels = sound.channels;
        let source_id = sound.source_id();
        let normalised_progress = sound.normalised_progress();
        let sound_msg = gui::ActiveSoundMessage::Start {
            source_id,
            position,
            channels,
            normalised_progress,
        };
        let msg = gui::AudioMonitorMessage::ActiveSound(id, sound_msg);
        self.gui_audio_monitor_msg_tx.try_send(msg).ok();
        self.sounds.insert(id, sound)
    }

    /// Update the sound associated with the given Id by applying the given function to it.
    pub fn update_sound<F>(&mut self, id: &sound::Id, update: F) -> bool
    where
        F: FnOnce(&mut Sound),
    {
        match self.sounds.get_mut(id) {
            None => false,
            Some(active) => {
                update(&mut active.sound);
                true
            },
        }
    }

    /// Update all sounds that are produced by the source type with the given `Id`.
    ///
    /// Returns the number of sounds that were updated.
    pub fn update_sounds_with_source<F>(&mut self, id: &source::Id, mut update: F) -> usize
    where
        F: FnMut(&sound::Id, &mut Sound),
    {
        let mut count = 0;
        for (id, sound) in self.sounds_mut().filter(|&(_, ref s)| s.source_id() == *id) {
            update(id, sound);
            count += 1;
        }
        count
    }

    /// Removes the sound and sends an `End` active sound message to the GUI.
    ///
    /// Returns `false` if the sound did not exist
    pub fn remove_sound(&mut self, id: sound::Id) -> bool {
        let removed = self.sounds.remove(&id);
        if let Some(sound) = removed {
            // Notify the gui.
            let sound_msg = gui::ActiveSoundMessage::End { sound };
            let msg = gui::AudioMonitorMessage::ActiveSound(id, sound_msg);
            self.gui_audio_monitor_msg_tx.try_send(msg).ok();

            // Notify the soundscape thread.
            let update = move |soundscape: &mut soundscape::Model| {
                soundscape.remove_active_sound(&id);
            };
            self.soundscape_tx.send(soundscape::UpdateFn::from(update).into()).ok();
            true
        } else {
            false
        }
    }

    /// An iterator yielding mutable access to all sounds currently playing.
    pub fn sounds_mut(&mut self) -> SoundsMut {
        let iter = self.sounds.iter_mut();
        SoundsMut { iter }
    }
}

/// The function given to nannou to use for rendering.
pub fn render(mut model: Model, mut buffer: Buffer) -> (Model, Buffer) {
    {
        let Model {
            master_volume,
            dbap_rolloff_db,
            ref soloed,
            ref mut frame_count,
            ref mut sounds,
            ref mut unmixed_samples,
            ref mut exhausted_sounds,
            ref mut installation_analyses,
            ref mut speakers,
            ref mut dbap_speakers,
            ref mut dbap_speaker_gains,
            ref gui_audio_monitor_msg_tx,
            ref osc_output_msg_tx,
            ref soundscape_tx,
            ref mut fft,
            ref mut fft_planner,
            ref mut fft_frequency_amplitudes_2,
        } = model;

        // Always silence the buffer to begin.
        for sample in buffer.iter_mut() {
            *sample = 0.0;
        }

        // For each sound, request `buffer.len()` number of frames and sum them onto the
        // relevant output channels.
        for (&sound_id, sound) in sounds.iter_mut() {

            // Update the GUI with the position of the sound.
            let source_id = sound.source_id();
            let position = sound.position;
            let channels = sound.channels;
            let normalised_progress = sound.normalised_progress();
            let update = gui::ActiveSoundMessage::Update {
                source_id,
                position,
                channels,
                normalised_progress,
            };
            let msg = gui::AudioMonitorMessage::ActiveSound(sound_id, update);
            gui_audio_monitor_msg_tx.try_send(msg).ok();

            let ActiveSound {
                ref mut sound,
                ref mut channel_detectors,
                ..
            } = *sound;

            // Don't play or request samples if paused.
            if !sound.shared.is_playing() {
                continue;
            }

            // The number of samples to request from the sound for this buffer.
            let num_samples = buffer.len_frames() * sound.channels;

            // Don't play it if some other sources are soloed.
            if sound.muted || (!soloed.is_empty() && !soloed.contains(&sound.source_id())) {
                // Pull samples from the signal but do not render them.
                let samples_yielded = sound.signal.samples().take(num_samples).count();
                if samples_yielded < num_samples {
                    exhausted_sounds.push(sound_id);
                }
                continue;
            }

            // If the source is a `Continuous` WAV, ensure it is seeked to the correct position.
            if let source::SignalKind::Wav { ref playback, ref mut samples } = sound.signal.kind {
                if let source::wav::Playback::Continuous = *playback {
                    if let Err(err) = samples.seek(*frame_count) {
                        eprintln!("failed to seek file for continuous WAV source: {}", err);
                        continue;
                    }
                }
            }

            // Clear the unmixed samples, ready to collect the new ones.
            unmixed_samples.clear();
            {
                let mut samples_written = 0;
                for sample in sound.signal.samples().take(num_samples) {
                    unmixed_samples.push(sample);
                    channel_detectors[samples_written % sound.channels].next(sample);
                    samples_written += 1;
                }

                // If we didn't write the expected number of samples, the sound has been exhausted.
                if samples_written < num_samples {
                    exhausted_sounds.push(sound_id);
                    for _ in samples_written..num_samples {
                        unmixed_samples.push(0.0);
                    }
                }

                // Send the latest RMS and peak for each channel to the GUI for monitoring.
                for (index, env_detector) in channel_detectors.iter().enumerate() {
                    let (rms, peak) = env_detector.current();
                    let sound_msg = gui::ActiveSoundMessage::UpdateChannel { index, rms, peak };
                    let msg = gui::AudioMonitorMessage::ActiveSound(sound_id, sound_msg);
                    gui_audio_monitor_msg_tx.try_send(msg).ok();
                }
            }

            // Mix the audio from the signal onto each of the output channels.
            for (i, channel_point) in sound.channel_points().enumerate() {
                // Update the dbap_speakers buffer with their distances to this sound channel.
                dbap_speakers.clear();
                for channel in 0..buffer.channels() {
                    // Find the speaker for this channel.
                    // TODO: Could speed this up by maintaining a map from channels to speaker IDs.
                    if let Some(active) = speakers.values().find(|s| s.speaker.channel == channel) {
                        let channel_point_f = Point2 {
                            x: channel_point.x.0,
                            y: channel_point.y.0,
                        };
                        let speaker = &active.speaker.point;
                        let speaker_f = Point2 {
                            x: speaker.x.0,
                            y: speaker.y.0,
                        };
                        let distance = dbap::blurred_distance_2(channel_point_f, speaker_f, DISTANCE_BLUR);
                        let weight = speaker::dbap_weight(&sound.installations, &active.speaker.installations);
                        dbap_speakers.push(dbap::Speaker { distance, weight });
                    }
                }

                // Update the speaker gains.
                dbap_speaker_gains.clear();
                let gains = dbap::SpeakerGains::new(&dbap_speakers, dbap_rolloff_db);
                dbap_speaker_gains.extend(gains.map(|f| f as f32));

                // For every frame in the buffer, mix the unmixed sample.
                let mut sample_index = i;
                for frame in buffer.frames_mut() {
                    let channel_sample = unmixed_samples[sample_index];
                    for (channel, &speaker_gain) in dbap_speaker_gains.iter().enumerate() {
                        // Only write to the channels that will be read by the audio device.
                        if let Some(sample) = frame.get_mut(channel) {
                            *sample += channel_sample * speaker_gain * sound.volume;
                        }
                    }
                    sample_index += sound.channels;
                }
            }
        }

        // For each speaker, feed its amplitude into its detectors.
        let n_channels = buffer.channels();
        let mut sum_peak = 0.0;
        let mut sum_rms = 0.0;
        let mut sum_lmh = [0.0; 3];
        let mut sum_fft_8_band = [0.0; 8];
        for (&id, active) in speakers.iter_mut() {
            let mut channel_i = active.speaker.channel;
            if channel_i >= n_channels {
                continue;
            }
            let ActiveSpeaker {
                ref mut env_detector,
                ref mut fft_detector,
                ..
            } = *active;
            for frame in buffer.frames() {
                let sample = frame[channel_i];
                env_detector.next(sample);
                fft_detector.push(sample);
            }

            // The current env and fft detector states.
            let (rms, peak) = env_detector.current();
            fft_detector.calc_fft(fft_planner, fft, &mut fft_frequency_amplitudes_2[..]);
            let (l_2, m_2, h_2) = fft::lmh(&fft_frequency_amplitudes_2[..]);
            let mut fft_8_bins_2 = [0.0; 8];
            fft::mel_bins(&fft_frequency_amplitudes_2[..], &mut fft_8_bins_2);

            // Send the detector state for the speaker to the GUI.
            let speaker_msg = gui::SpeakerMessage::Update { rms, peak };
            let msg = gui::AudioMonitorMessage::Speaker(id, speaker_msg);
            gui_audio_monitor_msg_tx.try_send(msg).ok();

            // Sum the rms and peak.
            for installation in &active.speaker.installations {
                let speakers = match installation_analyses.get_mut(&installation) {
                    None => continue,
                    Some(speakers) => speakers,
                };
                sum_peak += peak;
                sum_rms += rms;
                for (sum, amp_2) in sum_lmh.iter_mut().zip(&[l_2, m_2, h_2]) {
                    *sum += amp_2.sqrt() / (FFT_WINDOW_LEN / 2) as f32;
                }
                for (sum, amp_2) in sum_fft_8_band.iter_mut().zip(&fft_8_bins_2) {
                    *sum += amp_2.sqrt() / (FFT_WINDOW_LEN / 2) as f32;
                }
                let analysis = SpeakerAnalysis {
                    peak,
                    rms,
                    index: channel_i,
                };
                speakers.push(analysis);
            }
        }

        // Send the collected analysis to the OSC output thread.
        for (&installation, speakers) in installation_analyses.iter_mut() {
            if speakers.is_empty() {
                continue;
            }
            speakers.sort_by(|a, b| a.index.cmp(&b.index));
            let len_f = speakers.len() as f32;
            let avg_peak = sum_peak / len_f;
            let avg_rms = sum_rms / len_f;
            let avg_lmh = [sum_lmh[0] / len_f, sum_lmh[1] / len_f, sum_lmh[2] / len_f];
            let mut avg_8_band = [0.0; 8];
            for (avg, &sum) in avg_8_band.iter_mut().zip(&sum_fft_8_band) {
                *avg = sum / len_f;
            }
            let avg_fft = osc::output::FftData {
                lmh: avg_lmh,
                bins: avg_8_band,
            };
            let speakers = speakers
                .drain(..)
                .map(|s| osc::output::Speaker {
                    rms: s.rms,
                    peak: s.peak,
                })
                .collect();
            let data = osc::output::AudioFrameData {
                avg_peak,
                avg_rms,
                avg_fft,
                speakers,
            };
            let msg = osc::output::Message::Audio(installation, data);
            osc_output_msg_tx.send(msg).ok();
        }

        // Remove all sounds that have been exhausted.
        for sound_id in exhausted_sounds.drain(..) {
            // TODO: Possibly send this with the `End` message to avoid de-allocating on audio
            // thread.
            let sound = sounds.remove(&sound_id).unwrap();

            // Send signal of completion back to GUI thread.
            let sound_msg = gui::ActiveSoundMessage::End { sound };
            let msg = gui::AudioMonitorMessage::ActiveSound(sound_id, sound_msg);
            gui_audio_monitor_msg_tx.try_send(msg).ok();

            // Notify the soundscape thread.
            let update = move |soundscape: &mut soundscape::Model| {
                soundscape.remove_active_sound(&sound_id);
            };
            soundscape_tx.send(soundscape::UpdateFn::from(update).into()).ok();
        }

        // Apply the master volume.
        for sample in buffer.iter_mut() {
            *sample *= master_volume;
        }

        // Find the peak amplitude and send it via the monitor channel.
        let peak = buffer.iter().fold(0.0, |peak, &s| s.max(peak));
        gui_audio_monitor_msg_tx.try_send(gui::AudioMonitorMessage::Master { peak }).ok();

        // Step the frame count.
        *frame_count += buffer.len_frames() as u64;
    }

    (model, buffer)
}

pub fn channel_point(
    sound_point: Point2<Metres>,
    channel_index: usize,
    total_channels: usize,
    spread: Metres,
    radians: f32,
) -> Point2<Metres> {
    assert!(channel_index < total_channels);
    if total_channels == 1 {
        sound_point
    } else {
        let phase = channel_index as f32 / total_channels as f32;
        let channel_radians_offset = phase * std::f32::consts::PI * 2.0;
        let radians = (radians + channel_radians_offset) as f64;
        let (rel_x, rel_y) = utils::rad_mag_to_x_y(radians, spread.0);
        let x = sound_point.x + Metres(rel_x);
        let y = sound_point.y + Metres(rel_y);
        Point2 { x, y }
    }
}

/// Tests whether or not the given speaker position is within the `PROXIMITY_LIMIT` distance of the
/// given `point` (normally a `Sound`'s channel position).
pub fn speaker_is_in_proximity(point: &Point2<Metres>, speaker: &Point2<Metres>) -> bool {
    let point_f = Point2 {
        x: point.x.0,
        y: point.y.0,
    };
    let speaker_f = Point2 {
        x: speaker.x.0,
        y: speaker.y.0,
    };
    let distance_2 = Metres(point_f.distance2(speaker_f));
    distance_2 < PROXIMITY_LIMIT_2
}
