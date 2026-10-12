//! Call/system audio capture through Core Audio process taps (macOS 14.2+).
//!
//! A global stereo tap of every process except Parley is wrapped in a private
//! aggregate device; an IOProc on that device receives the mixed system output,
//! which is downmixed to mono, resampled to 16 kHz and sent to the recorder.
//! Nothing is muted: the user keeps hearing the call normally.
//!
//! Permission: the first tap triggers macOS's "System Audio Recording" prompt
//! (`NSAudioCaptureUsageDescription`). When access is denied the tap still
//! starts but delivers silence, so the status is checked separately with
//! `permission_status`.

use crossbeam_channel::Sender;

/// Downmix the tap's buffers to mono. `buffers` are the tap's own buffers:
/// either one interleaved buffer with `channels` > 1, or one mono buffer per
/// channel (non-interleaved).
pub fn downmix(buffers: &[(&[f32], u32)]) -> Vec<f32> {
    match buffers {
        [] => Vec::new(),
        [(data, channels)] => {
            let ch = (*channels).max(1) as usize;
            data.chunks_exact(ch)
                .map(|frame| frame.iter().sum::<f32>() / ch as f32)
                .collect()
        }
        many => {
            let frames = many.iter().map(|(d, _)| d.len()).min().unwrap_or(0);
            let n = many.len() as f32;
            (0..frames)
                .map(|i| many.iter().map(|(d, _)| d[i]).sum::<f32>() / n)
                .collect()
        }
    }
}

/// Status of the "System Audio Recording" privacy permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionStatus {
    Authorized,
    Denied,
    NotDetermined,
    /// The status can't be read on this system (the check uses a private API).
    Unknown,
    /// macOS older than 14.2, or not macOS.
    Unsupported,
}

impl PermissionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authorized => "authorized",
            Self::Denied => "denied",
            Self::NotDetermined => "not_determined",
            Self::Unknown => "unknown",
            Self::Unsupported => "unsupported",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [
            Self::Authorized,
            Self::Denied,
            Self::NotDetermined,
            Self::Unknown,
            Self::Unsupported,
        ]
        .into_iter()
        .find(|s| s.as_str() == value)
    }
}

/// `TCCAccessPreflight` is cached for the process lifetime, so a grant the
/// user just made isn't seen until restart. Spawn our own binary with
/// `--check-system-audio` to read a fresh status; fall back to the in-process
/// check if the subprocess can't be launched.
pub fn permission_status_fresh() -> PermissionStatus {
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(output) = std::process::Command::new(&exe)
            .arg("--check-system-audio")
            .output()
        {
            if output.status.success() {
                if let Some(status) =
                    PermissionStatus::parse(String::from_utf8_lossy(&output.stdout).trim())
                {
                    return status;
                }
            }
        }
    }
    permission_status()
}

#[cfg(target_os = "macos")]
pub use mac::{permission_status, SystemAudioCapture};

#[cfg(not(target_os = "macos"))]
pub struct SystemAudioCapture;

#[cfg(not(target_os = "macos"))]
impl SystemAudioCapture {
    pub fn start(_tx: Sender<Vec<i16>>) -> Result<Self, String> {
        Err("System audio capture needs macOS 14.2 or later".to_string())
    }
    pub fn stop(self) {}
}

#[cfg(not(target_os = "macos"))]
pub fn permission_status() -> PermissionStatus {
    PermissionStatus::Unsupported
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{downmix, PermissionStatus, Sender};
    use crate::audio::{f32_to_i16, AudioResampler, TARGET_SAMPLE_RATE};
    use objc2_06::rc::Retained;
    use objc2_06::runtime::AnyObject;
    use objc2_06::{AllocAnyThread, Message};
    use objc2_core_audio::{
        kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
        kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
        kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
        kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
        kAudioDevicePropertyDeviceUID, kAudioHardwarePropertyDefaultSystemOutputDevice,
        kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioSubDeviceUIDKey,
        kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyFormat,
        AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID,
        AudioDeviceStart, AudioDeviceStop, AudioHardwareCreateAggregateDevice,
        AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
        AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectID,
        AudioObjectPropertyAddress, CATapDescription, CATapMuteBehavior,
    };
    use objc2_core_audio_types::{
        kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, AudioBuffer, AudioBufferList,
        AudioStreamBasicDescription, AudioTimeStamp,
    };
    use objc2_core_foundation::CFDictionary;
    use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};
    use std::ffi::{c_void, CStr};
    use std::ptr::NonNull;

    struct ProcState {
        tx: Sender<Vec<i16>>,
        resampler: AudioResampler,
        /// How many of the input buffers (from the end) belong to the tap.
        tap_buffers: usize,
    }

    pub struct SystemAudioCapture {
        tap: AudioObjectID,
        aggregate: AudioObjectID,
        proc_id: AudioDeviceIOProcID,
        state: *mut ProcState,
    }

    // The raw pointers are only touched by the IOProc (Core Audio thread) and
    // by `stop`, after the device is stopped.
    unsafe impl Send for SystemAudioCapture {}

    fn address(selector: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        }
    }

    unsafe fn get_property<T: Copy>(
        object: AudioObjectID,
        selector: u32,
        qualifier: Option<(&c_void, u32)>,
        mut value: T,
    ) -> Result<T, String> {
        let mut addr = address(selector);
        let mut size = std::mem::size_of::<T>() as u32;
        let (q_size, q_ptr) = qualifier
            .map(|(p, s)| (s, p as *const c_void))
            .unwrap_or((0, std::ptr::null()));
        let status = AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut addr),
            q_size,
            q_ptr,
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        );
        if status != 0 {
            return Err(format!(
                "Core Audio property {} failed ({status})",
                fourcc(selector)
            ));
        }
        Ok(value)
    }

    fn fourcc(value: u32) -> String {
        value
            .to_be_bytes()
            .iter()
            .map(|b| {
                if b.is_ascii_graphic() {
                    *b as char
                } else {
                    '?'
                }
            })
            .collect()
    }

    fn key(name: &CStr) -> Retained<NSString> {
        NSString::from_str(name.to_str().unwrap_or_default())
    }

    fn any<T: Message>(value: Retained<T>) -> Retained<AnyObject> {
        // Every Objective-C object is an AnyObject.
        unsafe { Retained::cast_unchecked(value) }
    }

    fn dict(
        entries: Vec<(&CStr, Retained<AnyObject>)>,
    ) -> Retained<NSDictionary<NSString, AnyObject>> {
        let keys: Vec<Retained<NSString>> = entries.iter().map(|(k, _)| key(k)).collect();
        let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let values: Vec<Retained<AnyObject>> = entries.into_iter().map(|(_, v)| v).collect();
        NSDictionary::from_retained_objects(&key_refs, &values)
    }

    /// The audio-server object for our own process, so the tap can exclude it.
    fn own_process_object() -> Option<AudioObjectID> {
        let pid = std::process::id() as i32;
        let id: AudioObjectID = unsafe {
            get_property(
                kAudioObjectSystemObject as AudioObjectID,
                kAudioHardwarePropertyTranslatePIDToProcessObject,
                Some((&*(&pid as *const i32 as *const c_void), 4)),
                0u32,
            )
            .ok()?
        };
        (id != 0).then_some(id)
    }

    fn default_output_uid() -> Result<Retained<NSString>, String> {
        unsafe {
            let device: AudioObjectID = get_property(
                kAudioObjectSystemObject as AudioObjectID,
                kAudioHardwarePropertyDefaultSystemOutputDevice,
                None,
                0u32,
            )?;
            let uid: *mut NSString = get_property(
                device,
                kAudioDevicePropertyDeviceUID,
                None,
                std::ptr::null_mut(),
            )?;
            // The property returns a +1 CFStringRef (toll-free bridged).
            Retained::from_raw(uid).ok_or_else(|| "Output device has no UID".to_string())
        }
    }

    pub fn is_supported() -> bool {
        // AudioHardwareCreateProcessTap and CATapDescription arrived in 14.2.
        objc2_06::runtime::AnyClass::get(c"CATapDescription").is_some()
    }

    unsafe extern "C-unwind" fn io_proc(
        _device: AudioObjectID,
        _now: NonNull<AudioTimeStamp>,
        input: NonNull<AudioBufferList>,
        _input_time: NonNull<AudioTimeStamp>,
        _output: NonNull<AudioBufferList>,
        _output_time: NonNull<AudioTimeStamp>,
        client: *mut c_void,
    ) -> i32 {
        let state = &mut *(client as *mut ProcState);
        let list = input.as_ref();
        let count = list.mNumberBuffers as usize;
        let first = list.mBuffers.as_ptr();
        let take = state.tap_buffers.min(count);
        let mut buffers: Vec<(&[f32], u32)> = Vec::with_capacity(take);
        for i in (count - take)..count {
            let buffer: &AudioBuffer = &*first.add(i);
            if buffer.mData.is_null() {
                continue;
            }
            let len = buffer.mDataByteSize as usize / std::mem::size_of::<f32>();
            let data = std::slice::from_raw_parts(buffer.mData as *const f32, len);
            let channels = if take == 1 {
                buffer.mNumberChannels.max(1)
            } else {
                1
            };
            buffers.push((data, channels));
        }
        let mono = downmix(&buffers);
        let samples = f32_to_i16(&state.resampler.process(&mono));
        if !samples.is_empty() {
            let _ = state.tx.send(samples);
        }
        0
    }

    impl SystemAudioCapture {
        pub fn start(tx: Sender<Vec<i16>>) -> Result<Self, String> {
            if !is_supported() {
                return Err("System audio capture needs macOS 14.2 or later".to_string());
            }
            unsafe {
                let exclude: Vec<Retained<NSNumber>> = own_process_object()
                    .map(NSNumber::new_u32)
                    .into_iter()
                    .collect();
                let exclude = NSArray::from_retained_slice(&exclude);
                let desc = CATapDescription::initStereoGlobalTapButExcludeProcesses(
                    CATapDescription::alloc(),
                    &exclude,
                );
                desc.setName(&NSString::from_str("Parley"));
                desc.setPrivate(true);
                desc.setMuteBehavior(CATapMuteBehavior::Unmuted);
                let tap_uid = desc.UUID().UUIDString();

                let mut tap: AudioObjectID = 0;
                let status = AudioHardwareCreateProcessTap(Some(&desc), &mut tap);
                if status != 0 || tap == 0 {
                    return Err(format!("Could not create the system audio tap ({status})"));
                }
                let cleanup_tap = |tap| {
                    AudioHardwareDestroyProcessTap(tap);
                };

                let format: AudioStreamBasicDescription =
                    match get_property(tap, kAudioTapPropertyFormat, None, std::mem::zeroed()) {
                        Ok(f) => f,
                        Err(e) => {
                            cleanup_tap(tap);
                            return Err(e);
                        }
                    };
                if format.mFormatFlags & kAudioFormatFlagIsFloat == 0
                    || format.mBitsPerChannel != 32
                {
                    cleanup_tap(tap);
                    return Err(format!(
                        "Unsupported system audio format (flags {:#x}, {} bits)",
                        format.mFormatFlags, format.mBitsPerChannel
                    ));
                }
                let channels = format.mChannelsPerFrame.max(1);
                let tap_buffers = if format.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0 {
                    channels as usize
                } else {
                    1
                };

                let output_uid = match default_output_uid() {
                    Ok(uid) => uid,
                    Err(e) => {
                        cleanup_tap(tap);
                        return Err(e);
                    }
                };
                let aggregate_uid = NSUUID::new().UUIDString();
                let description = dict(vec![
                    (
                        kAudioAggregateDeviceNameKey,
                        any(NSString::from_str("Parley system audio")),
                    ),
                    (kAudioAggregateDeviceUIDKey, any(aggregate_uid)),
                    (
                        kAudioAggregateDeviceMainSubDeviceKey,
                        any(output_uid.clone()),
                    ),
                    (
                        kAudioAggregateDeviceIsPrivateKey,
                        any(NSNumber::new_bool(true)),
                    ),
                    (
                        kAudioAggregateDeviceIsStackedKey,
                        any(NSNumber::new_bool(false)),
                    ),
                    (
                        kAudioAggregateDeviceTapAutoStartKey,
                        any(NSNumber::new_bool(true)),
                    ),
                    (
                        kAudioAggregateDeviceSubDeviceListKey,
                        any(NSArray::from_retained_slice(&[dict(vec![(
                            kAudioSubDeviceUIDKey,
                            any(output_uid),
                        )])])),
                    ),
                    (
                        kAudioAggregateDeviceTapListKey,
                        any(NSArray::from_retained_slice(&[dict(vec![
                            (
                                kAudioSubTapDriftCompensationKey,
                                any(NSNumber::new_bool(true)),
                            ),
                            (kAudioSubTapUIDKey, any(tap_uid)),
                        ])])),
                    ),
                ]);
                let cf: &CFDictionary = &*(Retained::as_ptr(&description) as *const CFDictionary);
                let mut aggregate: AudioObjectID = 0;
                let status = AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut aggregate));
                if status != 0 || aggregate == 0 {
                    cleanup_tap(tap);
                    return Err(format!("Could not create the capture device ({status})"));
                }

                let state = Box::into_raw(Box::new(ProcState {
                    tx,
                    resampler: AudioResampler::new(
                        format.mSampleRate.round() as u32,
                        TARGET_SAMPLE_RATE,
                    ),
                    tap_buffers,
                }));
                let mut proc_id: AudioDeviceIOProcID = None;
                let status = AudioDeviceCreateIOProcID(
                    aggregate,
                    Some(io_proc),
                    state as *mut c_void,
                    NonNull::from(&mut proc_id),
                );
                if status != 0 || proc_id.is_none() {
                    AudioHardwareDestroyAggregateDevice(aggregate);
                    cleanup_tap(tap);
                    drop(Box::from_raw(state));
                    return Err(format!("Could not attach to the capture device ({status})"));
                }
                let status = AudioDeviceStart(aggregate, proc_id);
                if status != 0 {
                    AudioDeviceDestroyIOProcID(aggregate, proc_id);
                    AudioHardwareDestroyAggregateDevice(aggregate);
                    cleanup_tap(tap);
                    drop(Box::from_raw(state));
                    return Err(format!("Could not start system audio capture ({status})"));
                }
                log::info!(
                    "[system-audio] tap started: {} Hz, {} ch, {} buffer(s)",
                    format.mSampleRate,
                    channels,
                    tap_buffers
                );
                Ok(Self {
                    tap,
                    aggregate,
                    proc_id,
                    state,
                })
            }
        }

        pub fn stop(mut self) {
            self.teardown();
        }

        fn teardown(&mut self) {
            if self.state.is_null() {
                return;
            }
            unsafe {
                AudioDeviceStop(self.aggregate, self.proc_id);
                AudioDeviceDestroyIOProcID(self.aggregate, self.proc_id);
                AudioHardwareDestroyAggregateDevice(self.aggregate);
                AudioHardwareDestroyProcessTap(self.tap);
                // The IOProc can no longer run: free its state (drops the sender).
                drop(Box::from_raw(self.state));
            }
            self.state = std::ptr::null_mut();
            log::info!("[system-audio] tap stopped");
        }
    }

    impl Drop for SystemAudioCapture {
        fn drop(&mut self) {
            self.teardown();
        }
    }

    extern "C" {
        fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
    }

    /// Reads the TCC status for `kTCCServiceAudioCapture`. There is no public
    /// API for this; `TCCAccessPreflight` is the private SPI Apple's own tools
    /// use. If it can't be loaded the status is `Unknown` (recording still works
    /// if the user grants access when prompted).
    pub fn permission_status() -> PermissionStatus {
        if !is_supported() {
            return PermissionStatus::Unsupported;
        }
        type Preflight = unsafe extern "C" fn(*const c_void, *const c_void) -> i32;
        unsafe {
            let handle = dlopen(
                c"/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC".as_ptr(),
                1, // RTLD_LAZY
            );
            if handle.is_null() {
                return PermissionStatus::Unknown;
            }
            let symbol = dlsym(handle, c"TCCAccessPreflight".as_ptr());
            if symbol.is_null() {
                return PermissionStatus::Unknown;
            }
            let preflight: Preflight = std::mem::transmute(symbol);
            let service = NSString::from_str("kTCCServiceAudioCapture");
            match preflight(
                Retained::as_ptr(&service) as *const c_void,
                std::ptr::null(),
            ) {
                0 => PermissionStatus::Authorized,
                1 => PermissionStatus::Denied,
                2 => PermissionStatus::NotDetermined,
                _ => PermissionStatus::Unknown,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_status_round_trips_through_its_string() {
        for status in [
            PermissionStatus::Authorized,
            PermissionStatus::Denied,
            PermissionStatus::NotDetermined,
            PermissionStatus::Unknown,
            PermissionStatus::Unsupported,
        ] {
            assert_eq!(PermissionStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(PermissionStatus::parse("granted"), None);
    }

    #[test]
    fn interleaved_stereo_is_averaged_per_frame() {
        let data = [1.0, 0.0, 0.5, 0.5, -1.0, 1.0];
        assert_eq!(downmix(&[(&data, 2)]), vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn mono_buffer_passes_through() {
        let data = [0.1, 0.2];
        assert_eq!(downmix(&[(&data, 1)]), vec![0.1, 0.2]);
        // A zero channel count is treated as mono.
        assert_eq!(downmix(&[(&data, 0)]), vec![0.1, 0.2]);
    }

    #[test]
    fn non_interleaved_buffers_are_averaged_sample_wise() {
        let left = [1.0, 0.0, 0.25];
        let right = [0.0, 1.0];
        assert_eq!(downmix(&[(&left, 1), (&right, 1)]), vec![0.5, 0.5]);
        assert!(downmix(&[]).is_empty());
    }

    #[test]
    fn trailing_partial_frame_is_dropped() {
        let data = [1.0, 1.0, 0.5];
        assert_eq!(downmix(&[(&data, 2)]), vec![1.0]);
    }
}
