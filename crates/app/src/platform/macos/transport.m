// The Metal window retains its last frame while Core Animation paints the steady seek
// fill and elapsed label. Slint owns layout, formatting, accessibility and all interaction.
#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
#import <Metal/Metal.h>

static CAMetalLayer *metalSurface(CALayer *root) {
    if ([root isKindOfClass:CAMetalLayer.class] && ((CAMetalLayer *)root).device) return (CAMetalLayer *)root;
    for (CALayer *layer in root.sublayers)
        if ([layer isKindOfClass:CAMetalLayer.class] && ((CAMetalLayer *)layer).device) return (CAMetalLayer *)layer;
    return nil;
}

int mp_macos_window_surface(void *rawView) {
    CALayer *root = ((__bridge NSView *)rawView).layer;
    if (metalSurface(root)) return 2;
    for (CALayer *layer in root.sublayers)
        if ([layer.name isEqualToString:@"MarqueeSlintForeground"]) return 1;
    return 0;
}

@interface MPTransport : CALayer
@property(nonatomic, strong) CALayer *fill;
@property(nonatomic, strong) CALayer *clock;
@property(nonatomic) uint64_t clockRevision;
@end
@implementation MPTransport
@end

// The window's layer is the owner. Dropping a window also drops its presentation.
static __weak MPTransport *active;

static NSColor *paintColor(uint32_t argb) {
    return [NSColor colorWithSRGBRed:((argb >> 16) & 255)/255. green:((argb >> 8) & 255)/255.
                                blue:(argb & 255)/255. alpha:(argb >> 24)/255.];
}

bool mp_transport_paint(void *rawView, float scale, uint32_t pixelWidth, const float fill[4], const float clock[4],
                        const uint8_t *rgba, uint32_t w, uint32_t h, uint64_t revision, uint32_t ink) {
    CALayer *root = ((__bridge NSView *)rawView).layer;
    CAMetalLayer *front = metalSurface(root);
    MPTransport *layer = active;
    if (layer && (!rgba || layer.superlayer != root || !front)) {
        [layer removeFromSuperlayer];
        layer = nil;
    }
    if (!rgba || !front || !(root.bounds.size.width > 0) || !pixelWidth) return false;
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    if (!layer) {
        layer = [MPTransport layer];
        layer.geometryFlipped = front.geometryFlipped;
        layer.fill = [CALayer layer];
        layer.clock = [CALayer layer];
        [layer addSublayer:layer.fill];
        [layer addSublayer:layer.clock];
        if (front == root) [root addSublayer:layer];
        else [root insertSublayer:layer above:front];
        active = layer;
    }
    layer.frame = root.bounds;
    CGFloat pixels = pixelWidth / root.bounds.size.width;
    CGFloat p = scale / pixels;
    layer.fill.frame = CGRectMake(fill[0]*p, fill[1]*p, fill[2]*p, fill[3]*p);
    layer.fill.cornerRadius = fill[3]*p/2;
    layer.fill.backgroundColor = paintColor(ink).CGColor;
    CGFloat right = round((clock[0]+clock[2])*scale)/pixels;
    CGFloat top = round(clock[1]*scale)/pixels;
    layer.clock.frame = CGRectMake(right - w/pixels, top, w/pixels, h/pixels);
    layer.clock.contentsScale = pixels;
    if (layer.clockRevision != revision) {
        NSData *data = [NSData dataWithBytes:rgba length:(size_t)w*h*4];
        CGDataProviderRef provider = CGDataProviderCreateWithCFData((__bridge CFDataRef)data);
        CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
        CGImageRef image = CGImageCreate(w, h, 8, 32, (size_t)w*4, space,
                (CGBitmapInfo)kCGImageAlphaLast, provider, NULL, false, kCGRenderingIntentDefault);
        layer.clock.contents = (__bridge id)image;
        CGColorSpaceRelease(space);
        CGDataProviderRelease(provider);
        if (!image) {
            [layer removeFromSuperlayer];
            [CATransaction commit];
            return false;
        }
        CGImageRelease(image);
        layer.clockRevision = revision;
    }
    [CATransaction commit];
    return true;
}

#ifdef MP_BENCH
// Overlay the retained layers onto the usual straight-RGBA snapshot. The temporary
// bitmap covers only the narrow strip of changing pixels, regardless of window size.
void mp_transport_snapshot(uint8_t *rgba, uint32_t width, uint32_t height) {
    MPTransport *layer = active;
    CGSize size = layer.bounds.size;
    if (!layer.superlayer || !(size.width > 0 && size.height > 0) || !width || !height) return;
    CGFloat sx = width / size.width, sy = height / size.height;
    CGRect box = CGRectApplyAffineTransform(CGRectUnion(layer.clock.frame, layer.fill.frame),
                                            CGAffineTransformMakeScale(sx, sy));
    // Crop in top-down output pixels before rounding or clipping to the snapshot.
    if (layer.geometryFlipped) box.origin.y = height - CGRectGetMaxY(box);
    box = CGRectIntersection(CGRectIntegral(box), CGRectMake(0, 0, width, height));
    if (CGRectIsEmpty(box)) return;
    size_t w = (size_t)box.size.width, h = (size_t)box.size.height;
    CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef c = CGBitmapContextCreate(NULL, w, h, 8, w*4, space,
        (CGBitmapInfo)kCGImageAlphaPremultipliedLast);
    CGColorSpaceRelease(space);
    if (!c) return;
    // Match mp_spectrum_snapshot's full-window coordinates, cropped to this strip.
    if (!layer.geometryFlipped) {
        CGContextTranslateCTM(c, -box.origin.x, box.origin.y + h);
        CGContextScaleCTM(c, sx, -sy);
    } else {
        CGContextTranslateCTM(c, -box.origin.x, box.origin.y + h - height);
        CGContextScaleCTM(c, sx, sy);
    }
    [layer renderInContext:c];
    const uint8_t *src = CGBitmapContextGetData(c);
    for (size_t y = 0; y < h; ++y) {
        uint8_t *out = rgba + 4*((y + (size_t)box.origin.y)*width + (size_t)box.origin.x);
        for (size_t x = 0; x < w; ++x, src += 4, out += 4) {
            unsigned a = src[3];
            if (!a) continue;
            unsigned back = out[3]*(255-a), total = a*255 + back;
            for (unsigned i = 0; i < 3; ++i) {
                unsigned channel = (src[i]*255u*255 + out[i]*back + total/2) / total;
                out[i] = MIN(channel, 255u);
            }
            out[3] = (total + 127)/255;
        }
    }
    CGContextRelease(c);
}
#endif
