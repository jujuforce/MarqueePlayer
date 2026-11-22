#import <QuartzCore/QuartzCore.h>
#include <stdbool.h>
#include <stdint.h>

typedef struct { uint32_t op; float p[6]; } MPCommand;
typedef struct { const MPCommand *commands; size_t count; } MPPath;

// Interpolate a particle paint across audio samples, matching circles by their ID.
bool mp_particles_morph_paths(CAShapeLayer *shape, MPPath slice, double duration);
