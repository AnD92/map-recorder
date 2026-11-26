//! Core Audio FFI for the process tap. This is the only place in the crate
//! that uses `unsafe`.
//!
//! Lifecycle (see `docs/core-audio-tap.md` for the full explanation):
//! 1. describe a global stereo tap (`CATapDescription`);
//! 2. create it (`AudioHardwareCreateProcessTap`), which triggers the
//!    permission prompt the first time;
//! 3. read the tap's stream format;
//! 4. create a private aggregate device that contains the tap, because a tap
//!    can only be read through a device;
//! 5. install an IO block on the aggregate device and start it.
//!
//! Teardown happens in reverse order in [`Drop`], and also when setup fails
//! half-way.

use std::cell::RefCell;
use std::ffi::CStr;
use std::mem::size_of;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_core_audio::{
    AudioDeviceCreateIOProcIDWithBlock, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID,
    AudioDeviceStart, AudioDeviceStop, AudioHardwareCreateAggregateDevice,
    AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectID,
    AudioObjectPropertyAddress, CATapDescription, CATapMuteBehavior,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioSubDeviceUIDKey,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyFormat,
};
use objc2_core_audio_types::{
    AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, kAudioFormatFlagIsFloat,
    kAudioFormatLinearPCM,
};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};

use super::{CaptureError, StreamFormat};

/// Frames the interleaving scratch buffer can hold. Larger IO cycles are
/// processed in slices of this size, so the callback never allocates.
const SCRATCH_FRAMES: usize = 16_384;

type SampleCallback = Box<dyn FnMut(&[f32]) + Send>;
type IoBlock = RcBlock<
    dyn Fn(
        NonNull<AudioTimeStamp>,
        NonNull<AudioBufferList>,
        NonNull<AudioTimeStamp>,
        NonNull<AudioBufferList>,
        NonNull<AudioTimeStamp>,
    ),
>;

/// Owns every Core Audio object created for one capture session.
pub(super) struct TapSession {
    tap_id: AudioObjectID,
    aggregate_id: AudioObjectID,
    io_proc_id: AudioDeviceIOProcID,
    running: bool,
    format: StreamFormat,
    /// Kept alive for as long as Core Audio may invoke it.
    _io_block: Option<IoBlock>,
    /// IO cycles skipped because their buffer layout was not understood.
    unreadable_cycles: Arc<AtomicU64>,
}

// SAFETY: the session only holds plain Core Audio object IDs and a block
// that Core Audio invokes on its own thread; the IDs may be used from any
// thread, and the block's captured state is `Send`.
unsafe impl Send for TapSession {}

/// Converts a Core Audio status code into a `Result`.
fn check(call: &'static str, status: i32) -> Result<(), CaptureError> {
    if status == 0 {
        Ok(())
    } else {
        Err(CaptureError::CoreAudio { call, status })
    }
}

/// Builds the address of a global, main-element property.
fn global_address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Reads a fixed-size property value from a Core Audio object.
///
/// # Safety
/// `T` must be the exact plain-data type Core Audio stores for `selector`
/// (all-zero bytes must be a valid `T`).
unsafe fn read_property<T: Copy>(
    object: AudioObjectID,
    selector: u32,
    call: &'static str,
) -> Result<T, CaptureError> {
    let address = global_address(selector);
    // SAFETY: the caller guarantees `T` is plain data, valid when zeroed.
    let mut value: T = unsafe { std::mem::zeroed() };
    let mut size = size_of::<T>() as u32;
    // SAFETY: pointers reference live locals and `size` matches `T`, which the
    // caller guarantees is the property's type.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    check(call, status)?;
    Ok(value)
}

/// Returns the UID string of the current default output device, which the
/// aggregate device uses as its clock source.
fn default_output_device_uid() -> Result<Retained<NSString>, CaptureError> {
    // SAFETY: the default output device property is an `AudioObjectID`.
    let device: AudioObjectID = unsafe {
        read_property(
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyDefaultOutputDevice,
            "AudioObjectGetPropertyData(DefaultOutputDevice)",
        )?
    };
    // SAFETY: the device UID property is a `CFStringRef` returned with +1
    // retain count, stored here as a raw pointer.
    let raw: usize = unsafe {
        read_property(
            device,
            kAudioDevicePropertyDeviceUID,
            "AudioObjectGetPropertyData(DeviceUID)",
        )?
    };
    // SAFETY: `CFStringRef` is toll-free bridged to `NSString`, and we take
    // ownership of the +1 reference Core Audio handed us.
    unsafe { Retained::from_raw(raw as *mut NSString) }.ok_or(CaptureError::CoreAudio {
        call: "AudioObjectGetPropertyData(DeviceUID)",
        status: -1,
    })
}

/// Validates the tap's stream description and converts it into our format.
fn stream_format_from(asbd: &AudioStreamBasicDescription) -> Result<StreamFormat, CaptureError> {
    let is_float32 = asbd.mFormatID == kAudioFormatLinearPCM
        && asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0
        && asbd.mBitsPerChannel == 32;
    if !is_float32 || asbd.mChannelsPerFrame == 0 || asbd.mSampleRate <= 0.0 {
        return Err(CaptureError::UnsupportedFormat(format!(
            "format id {:#x}, flags {:#x}, {} bits, {} channels, {} Hz",
            asbd.mFormatID,
            asbd.mFormatFlags,
            asbd.mBitsPerChannel,
            asbd.mChannelsPerFrame,
            asbd.mSampleRate
        )));
    }
    Ok(StreamFormat {
        sample_rate: asbd.mSampleRate.round() as u32,
        channels: asbd.mChannelsPerFrame as u16,
    })
}

/// Converts one of Core Audio's C-string dictionary keys into an `NSString`.
fn key(name: &CStr) -> Retained<NSString> {
    NSString::from_str(name.to_str().expect("Core Audio keys are ASCII"))
}

/// Builds the description of a private aggregate device whose only input is
/// the tap, clocked by the current default output device.
fn aggregate_description(
    tap_uuid: &NSString,
    output_uid: &NSString,
) -> Retained<NSDictionary<NSString, AnyObject>> {
    let sub_device = NSDictionary::<NSString, AnyObject>::from_slices(
        &[&*key(kAudioSubDeviceUIDKey)],
        &[output_uid.as_ref()],
    );
    let sub_tap = NSDictionary::<NSString, AnyObject>::from_slices(
        &[
            &*key(kAudioSubTapUIDKey),
            &*key(kAudioSubTapDriftCompensationKey),
        ],
        &[tap_uuid.as_ref(), NSNumber::new_bool(true).as_ref()],
    );
    let aggregate_uid = NSUUID::new().UUIDString();
    let name = NSString::from_str("map-recorder system tap");
    let sub_devices = NSArray::from_slice(&[&*sub_device]);
    let taps = NSArray::from_slice(&[&*sub_tap]);
    let yes = NSNumber::new_bool(true);
    let no = NSNumber::new_bool(false);

    NSDictionary::from_slices(
        &[
            &*key(kAudioAggregateDeviceNameKey),
            &*key(kAudioAggregateDeviceUIDKey),
            &*key(kAudioAggregateDeviceMainSubDeviceKey),
            &*key(kAudioAggregateDeviceIsPrivateKey),
            &*key(kAudioAggregateDeviceIsStackedKey),
            &*key(kAudioAggregateDeviceTapAutoStartKey),
            &*key(kAudioAggregateDeviceSubDeviceListKey),
            &*key(kAudioAggregateDeviceTapListKey),
        ],
        &[
            name.as_ref(),
            aggregate_uid.as_ref(),
            output_uid.as_ref(),
            yes.as_ref(),
            no.as_ref(),
            yes.as_ref(),
            sub_devices.as_ref(),
            taps.as_ref(),
        ],
    )
}

/// Builds the IO block that runs on Core Audio's real-time thread.
///
/// IO cycles whose buffer layout cannot be understood increment
/// `unreadable` and are skipped.
///
/// The block reads the tap's input buffers, interleaves them into a
/// pre-allocated scratch buffer when Core Audio delivers one buffer per
/// channel, and forwards the samples to the user's callback. It never
/// allocates.
fn make_io_block(
    mut on_samples: SampleCallback,
    channels: usize,
    unreadable: Arc<AtomicU64>,
) -> IoBlock {
    let scratch = RefCell::new(vec![0.0f32; SCRATCH_FRAMES * channels]);
    let callback = RefCell::new(move |samples: &[f32]| on_samples(samples));

    RcBlock::new(
        move |_now: NonNull<AudioTimeStamp>,
              input: NonNull<AudioBufferList>,
              _input_time: NonNull<AudioTimeStamp>,
              _output: NonNull<AudioBufferList>,
              _output_time: NonNull<AudioTimeStamp>| {
            // Core Audio serialises IO cycles for a device, so these borrows
            // never overlap.
            let mut callback = callback.borrow_mut();
            let mut scratch = scratch.borrow_mut();

            // SAFETY: Core Audio guarantees `input` points to a valid buffer
            // list with `mNumberBuffers` entries for the duration of the call.
            let list = unsafe { input.as_ref() };
            let buffer_count = list.mNumberBuffers as usize;
            if buffer_count == 0 {
                return;
            }
            // SAFETY: `mBuffers` is a C flexible array of `mNumberBuffers` items.
            let buffers =
                unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), buffer_count) };

            // Find the tap's buffers; anything unexpected is counted so the
            // problem can be reported instead of failing silently.
            let layout = locate_tap_buffers(buffer_count, |i| buffers[i].mNumberChannels, channels);
            let first = match layout {
                Some(TapLayout::Interleaved(index)) => {
                    // A single buffer already holds every channel.
                    let buffer = &buffers[index];
                    if buffer.mData.is_null() {
                        unreadable.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    let len = buffer.mDataByteSize as usize / size_of::<f32>();
                    // SAFETY: the buffer holds `mDataByteSize` bytes of f32 samples.
                    let samples =
                        unsafe { std::slice::from_raw_parts(buffer.mData as *const f32, len) };
                    callback(samples);
                    return;
                }
                Some(TapLayout::NonInterleaved(first)) => first,
                None => {
                    unreadable.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };

            // One mono buffer per channel: interleave them slice by slice
            // into the scratch buffer.
            let buffers = &buffers[first..first + channels];
            if buffers.iter().any(|b| b.mData.is_null()) {
                unreadable.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let frames = buffers
                .iter()
                .map(|b| b.mDataByteSize as usize / size_of::<f32>())
                .min()
                .unwrap_or(0);
            let mut offset = 0;
            while offset < frames {
                let chunk = (frames - offset).min(SCRATCH_FRAMES);
                for (ch, buffer) in buffers.iter().enumerate() {
                    // SAFETY: each per-channel buffer holds at least `frames`
                    // f32 samples (checked via `mDataByteSize` above).
                    let channel = unsafe {
                        std::slice::from_raw_parts((buffer.mData as *const f32).add(offset), chunk)
                    };
                    for (frame, &sample) in channel.iter().enumerate() {
                        scratch[frame * channels + ch] = sample;
                    }
                }
                callback(&scratch[..chunk * channels]);
                offset += chunk;
            }
        },
    )
}

/// Where the tap's audio sits in the aggregate device's input buffer list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TapLayout {
    /// One buffer, at this index, holding all channels interleaved.
    Interleaved(usize),
    /// `channels` mono buffers starting at this index.
    NonInterleaved(usize),
}

/// Finds the tap's buffers among `count` input buffers whose channel counts
/// are given by `channels_of`.
fn locate_tap_buffers(
    count: usize,
    channels_of: impl Fn(usize) -> u32,
    channels: usize,
) -> Option<TapLayout> {
    // Sub-taps follow the sub-devices' own input streams (e.g. the inputs of
    // an audio interface used as output), so the tap is at the end of the list.
    if count == 0 || channels == 0 {
        return None;
    }
    if channels_of(count - 1) as usize == channels {
        return Some(TapLayout::Interleaved(count - 1));
    }
    if channels > 1 && count >= channels && (count - channels..count).all(|i| channels_of(i) == 1) {
        return Some(TapLayout::NonInterleaved(count - channels));
    }
    None
}

impl TapSession {
    /// Creates the tap and the aggregate device and starts streaming audio
    /// to `on_samples`. On failure everything created so far is released.
    pub(super) fn start(on_samples: SampleCallback) -> Result<Self, CaptureError> {
        // Process taps only exist on macOS 14.2+, where this class is defined.
        if AnyClass::get(c"CATapDescription").is_none() {
            return Err(CaptureError::UnsupportedOs);
        }

        let mut session = TapSession {
            tap_id: 0,
            aggregate_id: 0,
            io_proc_id: None,
            running: false,
            format: StreamFormat {
                sample_rate: 0,
                channels: 0,
            },
            _io_block: None,
            unreadable_cycles: Arc::new(AtomicU64::new(0)),
        };

        // Step 1: describe a private, unmuted stereo tap of every process.
        let exclude: Retained<NSArray<NSNumber>> = NSArray::new();
        // SAFETY: `CATapDescription` exists (checked above) and the init method
        // receives a valid, empty array.
        let description = unsafe {
            CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &exclude,
            )
        };
        let tap_uuid = NSUUID::new();
        // SAFETY: plain property setters on a valid description object.
        unsafe {
            description.setUUID(&tap_uuid);
            description.setName(&NSString::from_str("map-recorder"));
            description.setPrivate(true);
            description.setMuteBehavior(CATapMuteBehavior::Unmuted);
        }

        // Step 2: create the tap. The first call prompts for permission.
        // SAFETY: `description` is valid and `tap_id` is a writable location.
        let status =
            unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut session.tap_id) };
        check("AudioHardwareCreateProcessTap", status)?;

        // Step 3: learn the tap's sample rate and channel layout.
        // SAFETY: the tap format property is an `AudioStreamBasicDescription`.
        let asbd: AudioStreamBasicDescription = unsafe {
            read_property(
                session.tap_id,
                kAudioTapPropertyFormat,
                "AudioObjectGetPropertyData(TapFormat)",
            )?
        };
        session.format = stream_format_from(&asbd)?;

        // Step 4: wrap the tap in a private aggregate device.
        let output_uid = default_output_device_uid()?;
        let tap_uuid_string = tap_uuid.UUIDString();
        let description = aggregate_description(&tap_uuid_string, &output_uid);
        // SAFETY: `NSDictionary` is toll-free bridged to `CFDictionary`, and
        // the dictionary outlives the call.
        let cf_description = unsafe { &*(Retained::as_ptr(&description) as *const CFDictionary) };
        // SAFETY: valid dictionary and writable output location.
        let status = unsafe {
            AudioHardwareCreateAggregateDevice(
                cf_description,
                NonNull::from(&mut session.aggregate_id),
            )
        };
        check("AudioHardwareCreateAggregateDevice", status)?;

        // Step 5: install the real-time IO block and start the device.
        let block = make_io_block(
            on_samples,
            session.format.channels as usize,
            Arc::clone(&session.unreadable_cycles),
        );
        // SAFETY: the block is kept alive in `session` until the IO proc is
        // destroyed; a `None` queue makes Core Audio call it on its IO thread.
        let status = unsafe {
            AudioDeviceCreateIOProcIDWithBlock(
                NonNull::from(&mut session.io_proc_id),
                session.aggregate_id,
                None,
                RcBlock::as_ptr(&block),
            )
        };
        session._io_block = Some(block);
        check("AudioDeviceCreateIOProcIDWithBlock", status)?;

        // SAFETY: device and IO proc were created above.
        let status = unsafe { AudioDeviceStart(session.aggregate_id, session.io_proc_id) };
        check("AudioDeviceStart", status)?;
        session.running = true;

        Ok(session)
    }

    pub(super) fn format(&self) -> StreamFormat {
        self.format
    }

    pub(super) fn unreadable_cycles(&self) -> u64 {
        self.unreadable_cycles.load(Ordering::Relaxed)
    }
}

impl Drop for TapSession {
    /// Stops audio IO and releases Core Audio objects in reverse creation
    /// order. Errors are ignored: there is nothing useful to do with them
    /// while tearing down.
    fn drop(&mut self) {
        // SAFETY: every ID is either 0/None (never created) or a live object
        // created by `start`; each is released exactly once.
        unsafe {
            if self.running {
                AudioDeviceStop(self.aggregate_id, self.io_proc_id);
            }
            if self.io_proc_id.is_some() {
                AudioDeviceDestroyIOProcID(self.aggregate_id, self.io_proc_id);
            }
            if self.aggregate_id != 0 {
                AudioHardwareDestroyAggregateDevice(self.aggregate_id);
            }
            if self.tap_id != 0 {
                AudioHardwareDestroyProcessTap(self.tap_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locate(buffers: &[u32]) -> Option<TapLayout> {
        locate_tap_buffers(buffers.len(), |i| buffers[i], 2)
    }

    #[test]
    fn single_interleaved_buffer() {
        assert_eq!(locate(&[2]), Some(TapLayout::Interleaved(0)));
    }

    #[test]
    fn one_mono_buffer_per_channel() {
        assert_eq!(locate(&[1, 1]), Some(TapLayout::NonInterleaved(0)));
    }

    #[test]
    fn tap_follows_input_streams_of_the_output_device() {
        // e.g. an audio interface with a stereo input, then the stereo tap.
        assert_eq!(locate(&[2, 2]), Some(TapLayout::Interleaved(1)));
        assert_eq!(locate(&[1, 1, 1, 1]), Some(TapLayout::NonInterleaved(2)));
        assert_eq!(locate(&[8, 2]), Some(TapLayout::Interleaved(1)));
    }

    #[test]
    fn unknown_layouts_are_rejected() {
        assert_eq!(locate(&[]), None);
        assert_eq!(locate(&[1]), None);
        assert_eq!(locate(&[4]), None);
        assert_eq!(locate(&[2, 1]), None);
    }
}
