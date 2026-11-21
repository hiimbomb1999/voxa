#import <AppKit/AppKit.h>
#import <AVFoundation/AVFoundation.h>
#import <CoreAudio/CoreAudio.h>
#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/CATapDescription.h>
#import <unistd.h>

typedef void (*VoxaSamples)(const float *, const float *, size_t, size_t, uint32_t, void *);
typedef struct {
    AudioObjectID tap;
    AudioObjectID aggregate;
    AudioDeviceIOProcID io;
    VoxaSamples samples;
    void *context;
    uint32_t sampleRate;
} VoxaTap;

static AudioObjectPropertyAddress address(AudioObjectPropertySelector selector) {
    return (AudioObjectPropertyAddress){
        selector, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain
    };
}

static BOOL isBlackHoleUID(NSString *uid) {
    return [uid.lowercaseString containsString:@"blackhole"];
}

bool voxa_uses_blackhole(const char *deviceName) {
    @autoreleasepool {
        NSString *name = [NSString stringWithUTF8String:deviceName];
        AudioObjectPropertyAddress devices = address(kAudioHardwarePropertyDevices);
        UInt32 size = 0;
        if (AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &devices, 0, NULL, &size)) return false;
        NSMutableData *storage = [NSMutableData dataWithLength:size];
        if (AudioObjectGetPropertyData(kAudioObjectSystemObject, &devices, 0, NULL, &size, storage.mutableBytes)) return false;
        const AudioObjectID *ids = storage.bytes;
        for (size_t index = 0; index < size / sizeof(AudioObjectID); index++) {
            AudioObjectID device = ids[index];
            CFStringRef value = NULL;
            UInt32 length = sizeof(value);
            AudioObjectPropertyAddress property = address(kAudioObjectPropertyName);
            if (AudioObjectGetPropertyData(device, &property, 0, NULL, &length, &value)) continue;
            if (![CFBridgingRelease(value) isEqualToString:name]) continue;
            property = address(kAudioDevicePropertyDeviceUID);
            value = NULL;
            length = sizeof(value);
            if (!AudioObjectGetPropertyData(device, &property, 0, NULL, &length, &value)
                && isBlackHoleUID(CFBridgingRelease(value))) return true;
            property = address(kAudioAggregateDevicePropertyFullSubDeviceList);
            CFArrayRef subdevices = NULL;
            length = sizeof(subdevices);
            if (!AudioObjectGetPropertyData(device, &property, 0, NULL, &length, &subdevices)) {
                for (NSString *uid in CFBridgingRelease(subdevices)) {
                    if (isBlackHoleUID(uid)) return true;
                }
            }
        }
        return false;
    }
}

static NSArray<NSDictionary *> *processes(void) {
    AudioObjectPropertyAddress selector = address(kAudioHardwarePropertyProcessObjectList);
    UInt32 size = 0;
    if (AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &selector, 0, NULL, &size) || !size) {
        return @[];
    }
    NSMutableData *storage = [NSMutableData dataWithLength:size];
    if (AudioObjectGetPropertyData(kAudioObjectSystemObject, &selector, 0, NULL, &size, storage.mutableBytes)) {
        return @[];
    }

    NSMutableArray *result = [NSMutableArray array];
    const AudioObjectID *ids = storage.bytes;
    for (size_t index = 0; index < size / sizeof(AudioObjectID); index++) {
        pid_t pid = 0;
        UInt32 pidSize = sizeof(pid);
        AudioObjectPropertyAddress pidSelector = address(kAudioProcessPropertyPID);
        if (AudioObjectGetPropertyData(ids[index], &pidSelector, 0, NULL, &pidSize, &pid) || pid <= 0 || pid == getpid()) {
            continue;
        }
        NSRunningApplication *app = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
        CFStringRef audioBundle = NULL;
        UInt32 bundleSize = sizeof(audioBundle);
        AudioObjectPropertyAddress bundleSelector = address(kAudioProcessPropertyBundleID);
        OSStatus bundleStatus = AudioObjectGetPropertyData(
            ids[index], &bundleSelector, 0, NULL, &bundleSize, &audioBundle
        );
        NSString *bundle = bundleStatus == noErr && audioBundle ? CFBridgingRelease(audioBundle) : app.bundleIdentifier;
        NSString *key = bundle.length ? bundle : [NSString stringWithFormat:@"pid:%d", pid];
        [result addObject:@{
            @"key": key,
            @"name": app.localizedName ?: key,
            @"id": @(ids[index]),
        }];
    }
    return result;
}

char *voxa_sources(void) {
    @autoreleasepool {
        NSMutableArray *sources = [NSMutableArray array];
        NSMutableSet *seen = [NSMutableSet set];
        for (NSDictionary *process in processes()) {
            if ([seen containsObject:process[@"key"]]) continue;
            [seen addObject:process[@"key"]];
            [sources addObject:@{@"id": process[@"key"], @"name": process[@"name"]}];
        }
        NSData *json = [NSJSONSerialization dataWithJSONObject:sources options:0 error:nil];
        NSString *text = [[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding];
        return strdup(text.UTF8String ?: "[]");
    }
}

void voxa_free_string(char *value) {
    free(value);
}

int voxa_microphone_permission(void) {
    switch ([AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio]) {
        case AVAuthorizationStatusAuthorized: return 1;
        case AVAuthorizationStatusNotDetermined: return 0;
        default: return -1;
    }
}

void voxa_request_microphone(void (*callback)(bool, void *), void *context) {
    [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio completionHandler:^(BOOL granted) {
        callback(granted, context);
    }];
}

static OSStatus readAudio(
    AudioObjectID device, const AudioTimeStamp *now, const AudioBufferList *input,
    const AudioTimeStamp *inTime, AudioBufferList *output, const AudioTimeStamp *outTime, void *context
) {
    (void)device; (void)now; (void)inTime; (void)output; (void)outTime;
    VoxaTap *state = context;
    if (!input || !input->mNumberBuffers) return noErr;

    const AudioBuffer first = input->mBuffers[0];
    const size_t stride = first.mNumberChannels;
    if (!first.mData || !stride || stride > 2) return noErr;
    const float *left = first.mData;
    const float *right = stride == 2 ? left + 1 : left;
    size_t frames = first.mDataByteSize / (sizeof(float) * stride);
    if (stride == 1 && input->mNumberBuffers > 1) {
        const AudioBuffer second = input->mBuffers[1];
        if (second.mNumberChannels == 1 && second.mData) {
            right = second.mData;
            size_t rightFrames = second.mDataByteSize / sizeof(float);
            if (rightFrames < frames) frames = rightFrames;
        }
    }
    if (frames) state->samples(left, right, frames, stride, state->sampleRate, state->context);
    return noErr;
}

void voxa_stop_tap(VoxaTap *state) {
    if (!state) return;
    if (state->io) {
        AudioDeviceStop(state->aggregate, state->io);
        AudioDeviceDestroyIOProcID(state->aggregate, state->io);
    }
    if (state->aggregate) AudioHardwareDestroyAggregateDevice(state->aggregate);
    if (state->tap) AudioHardwareDestroyProcessTap(state->tap);
    free(state);
}

VoxaTap *voxa_start_tap(const char *source, bool monitor, VoxaSamples samples, void *context, double *sampleRate, int32_t *failure) {
    @autoreleasepool {
        NSMutableArray<NSNumber *> *selected = [NSMutableArray array];
        CATapDescription *desc = nil;
        if (source) {
            NSString *key = [NSString stringWithUTF8String:source];
            NSString *childPrefix = [key stringByAppendingString:@"."];
            for (NSDictionary *process in processes()) {
                NSString *processKey = process[@"key"];
                if ([processKey isEqualToString:key] || [processKey hasPrefix:childPrefix]) {
                    [selected addObject:process[@"id"]];
                }
            }
            if (!selected.count) {
                *failure = -1;
                return NULL;
            }
            desc = monitor ? [[CATapDescription alloc] initStereoMixdownOfProcesses:selected]
                           : [[CATapDescription alloc] initMonoMixdownOfProcesses:selected];
        } else {
            AudioObjectPropertyAddress selector = address(kAudioHardwarePropertyTranslatePIDToProcessObject);
            pid_t pid = getpid();
            AudioObjectID ownID = kAudioObjectUnknown;
            UInt32 size = sizeof(ownID);
            OSStatus status = AudioObjectGetPropertyData(kAudioObjectSystemObject, &selector, sizeof(pid), &pid, &size, &ownID);
            if (status || ownID == kAudioObjectUnknown) {
                *failure = -3;
                return NULL;
            }
            [selected addObject:@(ownID)];
            desc = monitor ? [[CATapDescription alloc] initStereoGlobalTapButExcludeProcesses:selected]
                           : [[CATapDescription alloc] initMonoGlobalTapButExcludeProcesses:selected];
        }
        desc.name = @"Voxa Translation Tap";
        desc.privateTap = YES;
        // The monitor already replays the original. Keep its direct route muted
        // for the lifetime of this tap, independently of the tap reader cadence.
        desc.muteBehavior = monitor ? CATapMuted : CATapUnmuted;

        VoxaTap *state = calloc(1, sizeof(*state));
        if (!state) {
            *failure = -4;
            return NULL;
        }
        state->samples = samples;
        state->context = context;
        OSStatus status = AudioHardwareCreateProcessTap(desc, &state->tap);
        AudioStreamBasicDescription format = {0};
        UInt32 size = sizeof(format);
        AudioObjectPropertyAddress selector = address(kAudioTapPropertyFormat);
        if (!status) status = AudioObjectGetPropertyData(state->tap, &selector, 0, NULL, &size, &format);
        if (!status && (format.mFormatID != kAudioFormatLinearPCM || format.mBitsPerChannel != 32 ||
                        !(format.mFormatFlags & kAudioFormatFlagIsFloat) || format.mChannelsPerFrame < 1 ||
                        format.mChannelsPerFrame > 2 ||
                        format.mSampleRate < 16000 || format.mSampleRate > 192000)) {
            status = -2;
        }
        if (!status) {
            NSDictionary *aggregate = @{
                @kAudioAggregateDeviceNameKey: @"Voxa Private Capture",
                @kAudioAggregateDeviceUIDKey: NSUUID.UUID.UUIDString,
                @kAudioAggregateDeviceIsPrivateKey: @YES,
                @kAudioAggregateDeviceTapAutoStartKey: @YES,
                @kAudioAggregateDeviceTapListKey: @[
                    @{
                        @kAudioSubTapUIDKey: desc.UUID.UUIDString,
                        @kAudioSubTapDriftCompensationKey: @YES,
                    }
                ],
            };
            status = AudioHardwareCreateAggregateDevice((__bridge CFDictionaryRef)aggregate, &state->aggregate);
        }
        state->sampleRate = (uint32_t)(format.mSampleRate + 0.5);
        if (!status) status = AudioDeviceCreateIOProcID(state->aggregate, readAudio, state, &state->io);
        if (!status) status = AudioDeviceStart(state->aggregate, state->io);
        if (status) {
            *failure = status;
            voxa_stop_tap(state);
            return NULL;
        }
        *sampleRate = format.mSampleRate;
        return state;
    }
}

char *voxa_save_dialog(const char *suggested) {
    @autoreleasepool {
        NSSavePanel *panel = [NSSavePanel savePanel];
        panel.title = @"Xuất transcript Voxa";
        panel.nameFieldStringValue = [NSString stringWithUTF8String:suggested];
        panel.canCreateDirectories = YES;
        if ([panel runModal] != NSModalResponseOK || !panel.URL.isFileURL) return NULL;
        return strdup(panel.URL.fileSystemRepresentation);
    }
}
