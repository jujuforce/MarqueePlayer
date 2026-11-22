#import <AppKit/AppKit.h>
#import <MediaPlayer/MediaPlayer.h>

// souvlaki publishes text and playback state on the main thread. Adding art to the
// current dictionary keeps any playback progress published while the file was loading.
static void publish(MPMediaItemArtwork *artwork) {
    MPNowPlayingInfoCenter *center = MPNowPlayingInfoCenter.defaultCenter;
    NSMutableDictionary *info = [center.nowPlayingInfo mutableCopy];
    if (info && artwork) {
        info[MPMediaItemPropertyArtwork] = artwork;
        center.nowPlayingInfo = info;
    }
}

// Accessed only on the main thread.
static NSURL *requested;
static MPMediaItemArtwork *current;
static uint64_t generation;

static NSURL *file_url(const char *path) {
    return path ? [NSURL fileURLWithFileSystemRepresentation:path isDirectory:NO relativeToURL:nil] : nil;
}

// souvlaki has just published the new metadata without artwork.
void marquee_set_artwork(const char *path) {
    NSURL *url = file_url(path);
    if (url && [requested isEqual:url]) {
        publish(current);
        return;
    }
    requested = url;
    current = nil;
    uint64_t ticket = ++generation;
    if (!url) return;

    dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
        @autoreleasepool {
            NSImage *image = [[NSImage alloc] initWithContentsOfURL:url];
            // The copied handler owns its image until the system releases the artwork.
            MPMediaItemArtwork *art = image ? [[MPMediaItemArtwork alloc] initWithBoundsSize:image.size
                requestHandler:^NSImage *(__unused CGSize size) { return image; }] : nil;
            dispatch_async(dispatch_get_main_queue(), ^{
                if (ticket != generation) return;
                current = art;
                if (art) publish(art);
                else requested = nil;
            });
        }
    });
}

#ifdef MP_BENCH
bool marquee_artwork_matches(const char *path) {
    MPMediaItemArtwork *shown = MPNowPlayingInfoCenter.defaultCenter.nowPlayingInfo[MPMediaItemPropertyArtwork];
    if (!path) return !shown && !current && !requested;
    return shown == current && [requested isEqual:file_url(path)] && [shown imageWithSize:shown.bounds.size] != nil;
}
#endif
