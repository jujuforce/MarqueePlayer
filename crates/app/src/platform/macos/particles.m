// Core Animation interpolates corresponding control points. A grain can cross from
// the dim paint to the bright paint, or disappear, between two samples. Match by ID
// so the remaining dots continue travelling along their own orbits.
#import "particles.h"
#import <objc/runtime.h>

static uint32_t particleID(const MPCommand *cmd) {
    uint32_t id;
    memcpy(&id, cmd->p + 3, sizeof(id));
    return id;
}

static void circlePath(CGMutablePathRef path, const MPCommand *cmd, bool visible) {
    CGFloat x = cmd->p[0], y = cmd->p[1], r = visible ? cmd->p[2] : 0;
    // Explicit controls keep even a zero-radius entrant or departing dot's topology.
    CGFloat c = 0.5522847498307936 * r;
    CGPathMoveToPoint(path, NULL, x+r, y);
    CGPathAddCurveToPoint(path, NULL, x+r, y+c, x+c, y+r, x, y+r);
    CGPathAddCurveToPoint(path, NULL, x-c, y+r, x-r, y+c, x-r, y);
    CGPathAddCurveToPoint(path, NULL, x-r, y-c, x-c, y-r, x, y-r);
    CGPathAddCurveToPoint(path, NULL, x+c, y-r, x+r, y-c, x+r, y);
    CGPathCloseSubpath(path);
}

bool mp_particles_morph_paths(CAShapeLayer *shape, MPPath next, double duration) {
    static char historyKey;
    for (size_t j = 0; j < next.count; ++j) if (next.commands[j].op != 6) return false;
    NSMutableData *history = objc_getAssociatedObject(shape, &historyKey);
    if (!history) {
        history = [NSMutableData data];
        objc_setAssociatedObject(shape, &historyKey, history, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    }
    const MPCommand *previous = history.bytes;
    size_t count = history.length / sizeof(MPCommand);
    if (!count && !next.count) return false;
    // Rust only initializes the active variant's fields. Compare each dot's x/y/r/ID,
    // ignoring the unused portion of the 28-byte command.
    if (count == next.count) {
        size_t unchanged = 0;
        while (unchanged < count && memcmp(previous[unchanged].p, next.commands[unchanged].p, 16) == 0) ++unchanged;
        if (unchanged == count) return true;
    }
    CGMutablePathRef from = duration > 0 && count ? CGPathCreateMutable() : NULL;
    CGMutablePathRef to = CGPathCreateMutable();
    size_t i = 0, j = 0;
    while (i < count || j < next.count) {
        const MPCommand *old = i < count ? previous+i : NULL;
        const MPCommand *now = j < next.count ? next.commands+j : NULL;
        // IDs follow birth order. Wrapping subtraction still orders active particles,
        // whose IDs are always less than half the 32-bit sequence apart.
        int32_t delta = old && now ? (int32_t)(particleID(old)-particleID(now)) : 0;
        if (old && (!now || delta < 0)) {
            if (from) {
                circlePath(from, old, true);
                circlePath(to, old, false);
            }
            ++i;
        } else if (now && (!old || delta > 0)) {
            if (from) circlePath(from, now, false);
            circlePath(to, now, true);
            ++j;
        } else {
            if (from) circlePath(from, old, true);
            circlePath(to, now, true);
            ++i;
            ++j;
        }
    }
    if (from) {
        CABasicAnimation *blend = [CABasicAnimation animationWithKeyPath:@"path"];
        blend.fromValue = (__bridge id)from;
        blend.toValue = (__bridge id)to;
        blend.duration = duration;
        if (@available(macOS 12.0, *)) blend.preferredFrameRateRange = CAFrameRateRangeMake(15, 60, 60);
        blend.timingFunction = [CAMediaTimingFunction functionWithName:kCAMediaTimingFunctionLinear];
        [shape addAnimation:blend forKey:@"spectrum"];
        CGPathRelease(from);
    }
    shape.path = to;
    CGPathRelease(to);
    history.length = next.count * sizeof(MPCommand);
    if (next.count) memcpy(history.mutableBytes, next.commands, history.length);
    return true;
}
