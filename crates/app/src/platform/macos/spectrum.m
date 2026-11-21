// Core Animation composites the spectrum between the page backdrop and Slint's
// premultiplied foreground. The original view still owns all input and focus.
#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
#import <Metal/Metal.h>
#import <CoreImage/CoreImage.h>
#import <dispatch/dispatch.h>
#import "particles.h"

bool mp_macos_has_metal_device(void) {
    return MTLCreateSystemDefaultDevice() != nil;
}

typedef struct { float position; uint32_t argb; } MPStop;
typedef struct { float x, width, height, radius, cap, reflection; } MPBar;
// Same order as visualizer/macos.rs and the paths in vizstage.slint.
enum { Bloom, Fill, Glow, Line, Caps, Reflection, Ripples = Reflection+6,
       SparksGlow, SparksOld, Sparks, FrontGlow, Front, LayerCount };
typedef struct { float x, y, w, h; } MPRect;
typedef struct { float blur, left, right, bar; MPRect pill, menu, submenu; float menu_radius; } MPGlass;

@interface MPPathMask : CAShapeLayer
@property(nonatomic) size_t sampleCount;
@end
@implementation MPPathMask
@end

@interface MPBarScene : CALayer
@property(nonatomic, strong) CALayer *art;
@property(nonatomic, strong) CAGradientLayer *tint;
@property(nonatomic, strong) CALayer *stage;
@property(nonatomic, strong) CALayer *panel;
@property(nonatomic) CGFloat points;
@property(nonatomic, strong) CAGradientLayer *bars;
@property(nonatomic, strong) CAGradientLayer *reflection;
@property(nonatomic, strong) CALayer *reflectionBox;
@property(nonatomic, strong) NSMutableDictionary<NSNumber *, CAGradientLayer *> *paints;
@property(nonatomic, strong) CALayer *coverShadow;
@property(nonatomic, weak) CALayer *foreground;
@property(nonatomic) size_t barCount;
@property(nonatomic) float baseline;
@property(nonatomic, strong) NSArray<CALayer *> *glassPanes;
@end
@implementation MPBarScene
@end

static MPBarScene *scene;

static CIFilter *glassBlur(void) {
    static CIFilter *blur;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ blur = [CIFilter filterWithName:@"CIGaussianBlur"]; });
    return blur;
}

static NSColor *color(uint32_t argb) {
    return [NSColor colorWithSRGBRed:((argb >> 16) & 255) / 255.0
                              green:((argb >> 8) & 255) / 255.0
                               blue:(argb & 255) / 255.0
                              alpha:(argb >> 24) / 255.0];
}

static CAGradientLayer *maskedGradient(CALayer *parent) {
    CAGradientLayer *gradient = [CAGradientLayer layer];
    gradient.mask = [CAShapeLayer layer];
    [parent addSublayer:gradient];
    return gradient;
}

// Called on the UI thread. The foreground may be recreated on hide or renderer change.
// 0 = incompatible surface, 1 = same scene, 2 = new scene needing its brushes and art.
int mp_spectrum_attach(void *rawView, bool menuOpen, bool glass) {
    NSView *view = (__bridge NSView *)rawView;
    if ((menuOpen && view.inLiveResize) || (glass && !glassBlur())) return 0;
    CALayer *root = view.layer;
    if (scene && scene.superlayer == root && scene.foreground.superlayer == root && root) return 1;
    [scene removeFromSuperlayer];
    scene = nil;
    CALayer *front = nil;
    for (CALayer *candidate in root.sublayers) {
        if ([candidate.name isEqualToString:@"MarqueeSlintForeground"] ||
            ([candidate isKindOfClass:CAMetalLayer.class] && ((CAMetalLayer *)candidate).device)) {
            front = candidate;
            break;
        }
    }
    if (!front) return 0;
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    scene = [MPBarScene layer];
    scene.foreground = front;
    scene.paints = [NSMutableDictionary dictionary];
    // AppKit already flips the backing layer with its NSView. Match the foreground's
    // top-left coordinates when installing a sibling (winit supplies a flipped view).
    scene.geometryFlipped = front.geometryFlipped;
    scene.masksToBounds = YES;
    scene.frame = root.bounds;
    scene.art = [CALayer layer];
    scene.art.contentsGravity = kCAGravityResizeAspectFill;
    scene.art.masksToBounds = YES;
    [scene addSublayer:scene.art];
    scene.tint = [CAGradientLayer layer];
    scene.tint.startPoint = CGPointMake(0.5, 0);
    scene.tint.endPoint = CGPointMake(0.5, 1);
    scene.tint.locations = @[@0, @0.7];
    [scene addSublayer:scene.tint];
    scene.panel = [CALayer layer];
    scene.panel.backgroundColor = color(0x40000000).CGColor;
    [scene addSublayer:scene.panel];
    scene.stage = [CALayer layer];
    [scene addSublayer:scene.stage];
    scene.bars = maskedGradient(scene.stage);
    scene.reflectionBox = [CALayer layer];
    [scene.stage addSublayer:scene.reflectionBox];
    scene.reflection = maskedGradient(scene.reflectionBox);
    [root insertSublayer:scene below:front];
    [CATransaction commit];
    return 2;
}

void mp_spectrum_remove(void) {
    [scene removeFromSuperlayer];
    scene = nil;
}

static CGImageRef copyRGB(const uint8_t *rgb, uint32_t width, uint32_t height) {
    CGImageRef image = NULL;
    if (rgb && width && height) {
        NSData *data = [NSData dataWithBytes:rgb length:(NSUInteger)width * height * 3];
        CGDataProviderRef source = CGDataProviderCreateWithCFData((__bridge CFDataRef)data);
        CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
        image = CGImageCreate(width, height, 8, 24, (size_t)width * 3, space, (CGBitmapInfo)kCGImageAlphaNone,
                              source, NULL, true, kCGRenderingIntentDefault);
        CGColorSpaceRelease(space);
        CGDataProviderRelease(source);
    }
    return image;
}

void mp_spectrum_backdrop(uint32_t base, uint32_t tint, float opacity,
                          const uint8_t *rgb, uint32_t width, uint32_t height) {
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    scene.backgroundColor = color(base).CGColor;
    scene.art.opacity = opacity;
    scene.tint.colors = @[(__bridge id)color(tint).CGColor, (__bridge id)color(base).CGColor];
    scene.tint.hidden = !rgb;
    CGImageRef image = copyRGB(rgb, width, height);
    scene.art.contents = (__bridge id)image;
    if (image) CGImageRelease(image);
    [CATransaction commit];
}

static void configureStops(CAGradientLayer *layer, const MPStop *stops, size_t count) {
    NSMutableArray *colors = [NSMutableArray arrayWithCapacity:count];
    NSMutableArray *locations = [NSMutableArray arrayWithCapacity:count];
    for (size_t i = 0; i < count; ++i) {
        [colors addObject:(__bridge id)color(stops[i].argb).CGColor];
        [locations addObject:@(stops[i].position)];
    }
    layer.colors = colors;
    layer.locations = locations;
}

void mp_spectrum_gradient(bool reflection, const MPStop *stops, size_t count, float angle) {
    CAGradientLayer *layer = reflection ? scene.reflection : scene.bars;
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    configureStops(layer, stops, count);
    layer.startPoint = angle == 90 ? CGPointMake(0,0.5) : CGPointMake(0.5, angle == 0 ? 1 : 0);
    layer.endPoint = angle == 90 ? CGPointMake(1,0.5) : CGPointMake(0.5, angle == 0 ? 0 : 1);
    [CATransaction commit];
}

// A reflected frequency gradient has the same horizontal colours in each slice.
// One static alpha mask supplies the progressively fainter strips.
void mp_spectrum_slices(const float *alpha, size_t count) {
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    CAGradientLayer *fade = nil;
    if (count) {
        fade = [CAGradientLayer layer];
        fade.frame = scene.reflectionBox.bounds;
        fade.startPoint = CGPointMake(0.5,0);
        fade.endPoint = CGPointMake(0.5,1);
        NSMutableArray *colors = [NSMutableArray arrayWithCapacity:count*2];
        NSMutableArray *locations = [NSMutableArray arrayWithCapacity:count*2];
        for (size_t i = 0; i < count; ++i) {
            id c = (__bridge id)[NSColor colorWithWhite:1 alpha:alpha[i]].CGColor;
            [colors addObjectsFromArray:@[c,c]];
            [locations addObjectsFromArray:@[@((double)i/count), @((double)(i+1)/count)]];
        }
        fade.colors = colors;
        fade.locations = locations;
    }
    scene.reflectionBox.mask = fade;
    [CATransaction commit];
}

void mp_spectrum_layout(float x, float y, float width, float top, float reflection,
                        float scale, uint32_t windowPixels) {
    CALayer *root = scene.superlayer;
    CGFloat points = root.bounds.size.width * scale / windowPixels;
    if (!(points > 0)) return;
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    scene.frame = root.bounds;
    scene.points = points;
    scene.art.frame = scene.bounds;
    scene.tint.frame = scene.bounds;
    scene.stage.anchorPoint = CGPointZero;
    scene.stage.position = CGPointMake(x * points, y * points);
    scene.stage.bounds = CGRectMake(0, 0, width, top + reflection);
    scene.stage.affineTransform = CGAffineTransformMakeScale(points, points);
    scene.bars.frame = CGRectMake(0, 0, width, top);
    scene.reflectionBox.frame = CGRectMake(0, top, width, reflection);
    scene.reflectionBox.mask.frame = scene.reflectionBox.bounds;
    scene.reflection.frame = scene.reflectionBox.bounds;
    scene.bars.mask.frame = scene.bars.bounds;
    scene.reflection.mask.frame = scene.reflection.bounds;
    scene.bars.mask.contentsScale = scale;
    scene.reflection.mask.contentsScale = scale;
    [CATransaction commit];
}

// The panel's rounded tint sits behind its spectrum and tab controls. Its frame
// comes from Slint's actual layout, including keyboard and window-size changes.
void mp_spectrum_panel(MPRect frame, float radius, bool clip) {
    CGFloat p = scene.points;
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    scene.panel.hidden = frame.w <= 0;
    scene.panel.frame = CGRectMake(frame.x*p, frame.y*p, frame.w*p, frame.h*p);
    scene.panel.cornerRadius = radius*p;
    scene.stage.masksToBounds = clip;
    [CATransaction commit];
}

// The native planes blur the live spectrum and backdrop. Skia still blurs page
// content on its own foreground plane and supplies the exact panel tint and border.
void mp_spectrum_glass(MPGlass panes) {
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    for (CALayer *pane in scene.glassPanes) [pane removeFromSuperlayer];
    scene.glassPanes = nil;
    if (panes.blur > 0) {
        CGFloat p = scene.points, w = scene.bounds.size.width, h = scene.bounds.size.height;
        CGFloat left = p * panes.left, right = p * panes.right, bar = p * panes.bar;
        CGRect boxes[] = {
            CGRectMake(0, 0, left, h - bar), CGRectMake(w - right, 0, right, h - bar),
            CGRectMake(0, h - bar, w, bar),
            CGRectMake(p * panes.pill.x, p * panes.pill.y, p * panes.pill.w, p * panes.pill.h),
            CGRectMake(p * panes.menu.x, p * panes.menu.y, p * panes.menu.w, p * panes.menu.h),
            CGRectMake(p * panes.submenu.x, p * panes.submenu.y, p * panes.submenu.w, p * panes.submenu.h),
        };
        NSMutableArray<CALayer *> *layers = [NSMutableArray arrayWithCapacity:6];
        for (unsigned i = 0; i < sizeof(boxes)/sizeof(boxes[0]); i++) {
            if (CGRectIsEmpty(boxes[i])) continue;
            CALayer *pane = [CALayer layer];
            pane.frame = boxes[i];
            pane.masksToBounds = YES;
            if (i == 3) pane.cornerRadius = boxes[i].size.height / 2;
            if (i >= 4) pane.cornerRadius = panes.menu_radius * p;
            CIFilter *blur = [glassBlur() copy];
            [blur setValue:@(panes.blur * p * scene.superlayer.contentsScale) forKey:kCIInputRadiusKey];
            pane.backgroundFilters = @[blur];
            [scene addSublayer:pane];
            [layers addObject:pane];
        }
        scene.glassPanes = layers;
    }
    [CATransaction commit];
}

// Keep the same path elements even for a silent band or absent cap: Core Animation
// interpolates corresponding control points as the next spectrum arrives.
static void boxPath(CGMutablePathRef path, CGFloat x, CGFloat y, CGFloat width, CGFloat height) {
    CGPathMoveToPoint(path, NULL, x,y);
    CGPathAddLineToPoint(path, NULL, x+width,y);
    CGPathAddLineToPoint(path, NULL, x+width,y+height);
    CGPathAddLineToPoint(path, NULL, x,y+height);
    CGPathCloseSubpath(path);
}

static void interpolate(CALayer *layer, NSString *key, id from, id to, double duration) {
    if (duration <= 0 || !from) return;
    CABasicAnimation *blend = [CABasicAnimation animationWithKeyPath:key];
    blend.fromValue = from;
    blend.toValue = to;
    blend.duration = duration;
    if (@available(macOS 12.0, *)) blend.preferredFrameRateRange = CAFrameRateRangeMake(15,60,60);
    blend.timingFunction = [CAMediaTimingFunction functionWithName:kCAMediaTimingFunctionLinear];
    [layer addAnimation:blend forKey:key];
}

static void publishPath(CAShapeLayer *shape, CGPathRef path, double duration) {
    if (path && duration > 0) interpolate(shape,@"path",(__bridge id)shape.path,(__bridge id)path,duration);
    shape.path = path;
}

void mp_spectrum_bars(const MPBar *bars, size_t count, float top, double duration) {
    CGMutablePathRef body = CGPathCreateMutable();
    CGMutablePathRef reflection = CGPathCreateMutable();
    for (size_t i = 0; i < count; ++i) {
        MPBar b = bars[i];
        CGFloat x = b.x, right = x + b.width, y = top - b.height, r = b.radius;
        CGPathMoveToPoint(body, NULL, x, top);
        CGPathAddLineToPoint(body, NULL, x, y + r);
        CGPathAddQuadCurveToPoint(body, NULL, x, y, x+r, y);
        CGPathAddLineToPoint(body, NULL, right-r, y);
        CGPathAddQuadCurveToPoint(body, NULL, right, y, right, y+r);
        CGPathAddLineToPoint(body, NULL, right, top);
        CGPathCloseSubpath(body);
        boxPath(body, x, b.cap >= 0 ? b.cap : y, b.width, b.cap >= 0 ? 2 : 0);
        boxPath(reflection, x, 1, b.width, MAX(0,b.reflection-1));
    }
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    if (scene.barCount != count || scene.baseline != top) duration = 0;
    publishPath((CAShapeLayer *)scene.bars.mask, body, duration);
    publishPath((CAShapeLayer *)scene.reflection.mask, reflection, duration);
    scene.barCount = count;
    scene.baseline = top;
    [CATransaction commit];
    CGPathRelease(body);
    CGPathRelease(reflection);
}

// Brushes are installed only when a look, its geometry or colours change. The paint
// can extend beyond the stage for the ring's outer sparks; path coordinates stay local.
void mp_spectrum_paint(uint32_t index, uint32_t kind, MPRect rect, MPRect axis,
                       float stroke, float scale, const MPStop *stops, size_t count) {
    CAGradientLayer *paint = scene.paints[@(index)];
    if (!count) {
        [paint removeFromSuperlayer];
        [scene.paints removeObjectForKey:@(index)];
        return;
    }
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    if (!paint) {
        paint = [CAGradientLayer layer];
        paint.zPosition = index;
        paint.hidden = index != Bloom;
        [scene.stage addSublayer:paint];
        scene.paints[@(index)] = paint;
        if (index != Bloom) paint.mask = [MPPathMask layer];
    }
    paint.frame = CGRectMake(rect.x, rect.y, rect.w, rect.h);
    paint.type = kind == 2 ? kCAGradientLayerRadial : kind == 3 ? kCAGradientLayerConic : kCAGradientLayerAxial;
    paint.startPoint = CGPointMake(axis.x, axis.y);
    paint.endPoint = CGPointMake(axis.w, axis.h);
    configureStops(paint, stops, count);
    CAShapeLayer *shape = (CAShapeLayer *)paint.mask;
    shape.frame = CGRectMake(-rect.x, index >= Reflection && index < Ripples ? 0 : -rect.y, rect.w, rect.h);
    shape.contentsScale = scale;
    // A new layout or look needs a new stationary ring as well as new brushes.
    shape.path = nil;
    shape.fillColor = stroke > 0 ? nil : NSColor.blackColor.CGColor;
    shape.strokeColor = stroke > 0 ? NSColor.blackColor.CGColor : nil;
    shape.lineWidth = stroke;
    shape.lineCap = kCALineCapRound;
    shape.lineJoin = kCALineJoinRound;
    [CATransaction commit];
}

static CGPathRef copyPath(MPPath path) {
    CGMutablePathRef result = CGPathCreateMutable();
    for (size_t i = 0; i < path.count; ++i) {
        const MPCommand *cmd = &path.commands[i];
        const float *p = cmd->p;
        switch (cmd->op) {
            case 0: CGPathMoveToPoint(result, NULL, p[0],p[1]); break;
            case 1: CGPathAddLineToPoint(result, NULL, p[0],p[1]); break;
            case 2: CGPathAddCurveToPoint(result, NULL, p[0],p[1],p[2],p[3],p[4],p[5]); break;
            case 3: CGPathCloseSubpath(result); break;
            case 6: // Tagged particle: the fourth payload word carries its stable identity.
            case 4: CGPathAddEllipseInRect(result, NULL, CGRectMake(p[0]-p[2],p[1]-p[2],2*p[2],2*p[2])); break;
            case 5: CGPathAddRect(result, NULL, CGRectMake(p[0],p[1],p[2],p[3])); break;
        }
    }
    return result;
}

void mp_spectrum_paths(const MPPath *paths, float pulse, float punch, bool ring, double duration, bool stationaryRim) {
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    for (NSNumber *key in scene.paints) {
        unsigned index = key.unsignedIntValue;
        CAGradientLayer *paint = scene.paints[key];
        if (index == Bloom) {
            if (paint.opacity != pulse) interpolate(paint,@"opacity",@(paint.opacity),@(pulse),duration);
            paint.opacity = pulse;
            continue;
        }
        MPPath slice = paths[index];
        if (index>=SparksGlow && (slice.count || !paint.hidden)) {
            CAShapeLayer *mask = (CAShapeLayer *)paint.mask;
            if (!slice.count && (!mask.path || CGPathIsEmpty(mask.path))) { paint.hidden = YES; continue; }
            if (mp_particles_morph_paths(mask,slice,duration)) {
                paint.hidden = NO;
                continue;
            }
        }
        if (slice.count == 0 && paint.hidden) continue;
        if (stationaryRim && (index == Line || index == Glow) && slice.count && !paint.hidden &&
            ((CAShapeLayer *)paint.mask).path) continue;
        paint.hidden = slice.count == 0;
        CAShapeLayer *shape = (CAShapeLayer *)paint.mask;
        // A halo's filled corona encloses its circular cutout.
        shape.fillRule = index == Fill && ring ? kCAFillRuleEvenOdd : kCAFillRuleNonZero;
        CGPathRef path = slice.count ? copyPath(slice) : NULL;
        // The line and area follow a fixed sequence of frequency bands. Preserve
        // that correspondence only while the stage layout and topology match.
        size_t previousCount = ((MPPathMask *)shape).sampleCount;
        if (index == Line || index == Glow || index == Fill || (index>=Reflection && index<Ripples)) {
            publishPath(shape,path,previousCount == slice.count ? duration : 0);
            ((MPPathMask *)shape).sampleCount = slice.count;
        } else { shape.path = path; }
        if (path) CGPathRelease(path);
    }
    if (scene.coverShadow && scene.coverShadow.affineTransform.a != punch) {
        interpolate(scene.coverShadow,@"transform.scale",@(scene.coverShadow.affineTransform.a),@(punch),duration);
        scene.coverShadow.affineTransform = CGAffineTransformMakeScale(punch, punch);
    }
    [CATransaction commit];
}

void mp_spectrum_cover(const uint8_t *rgb, uint32_t width, uint32_t height, float x, float y,
                       float size, float radius, bool behind) {
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    [scene.coverShadow removeFromSuperlayer];
    scene.coverShadow = nil;
    if (rgb) {
        CALayer *shadow = [CALayer layer];
        shadow.bounds = CGRectMake(0,0,size,size);
        shadow.position = CGPointMake(x,y);
        shadow.zPosition = Sparks + 0.5;
        shadow.shadowColor = NSColor.blackColor.CGColor;
        shadow.shadowOpacity = (behind ? 192.0 : 144.0) / 255.0;
        shadow.shadowRadius = behind ? 20 : 12;
        shadow.shadowOffset = CGSizeMake(0, behind ? 12 : 4);
        CGPathRef clip = CGPathCreateWithRoundedRect(shadow.bounds,radius,radius,NULL);
        shadow.shadowPath = clip;
        CGPathRelease(clip);
        [scene.stage addSublayer:shadow];
        CALayer *image = [CALayer layer];
        image.frame = shadow.bounds;
        image.contentsGravity = kCAGravityResizeAspectFill;
        image.masksToBounds = YES;
        image.cornerRadius = radius;
        CGImageRef cover = copyRGB(rgb,width,height);
        image.contents = (__bridge id)cover;
        if (cover) CGImageRelease(cover);
        [shadow addSublayer:image];
        scene.coverShadow = shadow;
    }
    [CATransaction commit];
}

// A preset change discards the previous paint masks, leaving the retained page backdrop.
void mp_spectrum_clear_geometry(void) {
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    for (CALayer *paint in scene.paints.allValues) [paint removeFromSuperlayer];
    [scene.paints removeAllObjects];
    ((CAShapeLayer *)scene.bars.mask).path = nil;
    ((CAShapeLayer *)scene.reflection.mask).path = nil;
    [scene.coverShadow removeFromSuperlayer];
    scene.coverShadow = nil;
    [CATransaction commit];
}

#ifdef MP_BENCH

float mp_spectrum_cover_scale(void) {
    return scene.coverShadow ? scene.coverShadow.affineTransform.a : 1.0;
}

// Slint's own snapshot contains only the foreground while native bars are active.
int mp_spectrum_snapshot(uint8_t *rgba, uint32_t width, uint32_t height) {
    CALayer *root = scene.superlayer;
    CGSize size = root.bounds.size;
    if (!root || !(size.width > 0 && size.height > 0) || !rgba) return false;
    CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef context = CGBitmapContextCreate(rgba, width, height, 8, (size_t)width * 4,
                                                space, (CGBitmapInfo)kCGImageAlphaPremultipliedLast);
    CGColorSpaceRelease(space);
    if (!context) return false;
    // The bitmap's first row is the top of the saved image. A flipped AppKit
    // backing layer has the opposite orientation when asked to render in a CGContext.
    if (!scene.geometryFlipped) {
        CGContextTranslateCTM(context, 0, height);
        CGContextScaleCTM(context, 1, -1);
    }
    CGContextScaleCTM(context, width / size.width, height / size.height);
    // The model tree contains the software tiles, but a CAMetalLayer drawable
    // cannot be read by renderInContext. Its renderer supplies that plane separately.
    bool metal = [scene.foreground isKindOfClass:CAMetalLayer.class];
    [(metal ? (CALayer *)scene : root) renderInContext:context];
    CGContextRelease(context);
    return metal ? 2 : 1;
}
#endif
