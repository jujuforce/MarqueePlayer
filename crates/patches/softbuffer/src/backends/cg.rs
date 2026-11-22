//! Softbuffer implementation using CoreGraphics.
use crate::backend_interface::*;
use crate::error::InitError;
use crate::{util, Rect, SoftBufferError};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass, MainThreadMarker, Message};
use objc2_core_foundation::{CFRetained, CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, CGImageComponentInfo, CGImagePixelFormatInfo,
};
use objc2_foundation::{
    ns_string, NSDictionary, NSKeyValueChangeKey, NSKeyValueChangeNewKey,
    NSKeyValueObservingOptions, NSNumber, NSObject, NSObjectNSKeyValueObserverRegistration,
    NSString, NSValue,
};
use objc2_quartz_core::{kCAGravityTopLeft, CALayer, CATransaction};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawWindowHandle};

use std::cell::Cell;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::size_of;
use std::num::NonZeroU32;
use std::ops::Deref;
use std::ptr::{self, NonNull};
use std::sync::Arc;

thread_local! {
    static PREMULTIPLIED_LAYER: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// Opt AppKit surfaces created on this thread into premultiplied ARGB pixels and
/// give their compositing layer a name. Other surfaces keep the usual XRGB format.
pub fn set_macos_premultiplied_layer(name: Option<&'static str>) {
    PREMULTIPLIED_LAYER.set(name);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SoftbufferObserver"]
    #[ivars = SendCALayer]
    #[derive(Debug)]
    struct Observer;

    /// NSKeyValueObserving
    impl Observer {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            key_path: Option<&NSString>,
            _object: Option<&AnyObject>,
            change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
            _context: *mut c_void,
        ) {
            self.update(key_path, change);
        }
    }
);

impl Observer {
    fn new(layer: &CALayer) -> Retained<Self> {
        let this = Self::alloc().set_ivars(SendCALayer(layer.retain()));
        unsafe { msg_send![super(this), init] }
    }

    fn update(
        &self,
        key_path: Option<&NSString>,
        change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
    ) {
        let layer = self.ivars();

        let change =
            change.expect("requested a change dictionary in `addObserver`, but none was provided");
        let new = change
            .objectForKey(unsafe { NSKeyValueChangeNewKey })
            .expect("requested change dictionary did not contain `NSKeyValueChangeNewKey`");

        // NOTE: Setting these values usually causes a quarter second animation to occur, which is
        // undesirable.
        //
        // However, since we're setting them inside an observer, there already is a transaction
        // ongoing, and as such we don't need to wrap this in a `CATransaction` ourselves.

        if key_path == Some(ns_string!("contentsScale")) {
            let new = new.downcast::<NSNumber>().unwrap();
            let scale_factor = new.as_cgfloat();

            // Set the scale factor of the layer to match the root layer when it changes (e.g. if
            // moved to a different monitor, or monitor settings changed).
            layer.setContentsScale(scale_factor);
        } else if key_path == Some(ns_string!("bounds")) {
            let new = new.downcast::<NSValue>().unwrap();
            let bounds = new.get_rect().expect("new bounds value was not CGRect");

            // Set `bounds` and `position` so that the new layer is inside the superlayer.
            //
            // This differs from just setting the `bounds`, as it also takes into account any
            // translation that the superlayer may have that we'd want to preserve.
            layer.setFrame(bounds);
        } else {
            panic!("unknown observed keypath {key_path:?}");
        }
    }
}

#[derive(Debug)]
pub struct CGImpl<D, W> {
    /// Our layer.
    layer: SendCALayer,
    /// The layer that our layer was created from.
    ///
    /// Can also be retrieved from `layer.superlayer()`.
    root_layer: SendCALayer,
    observer: Retained<Observer>,
    color_space: CFRetained<CGColorSpace>,
    /// The width of the underlying buffer.
    width: usize,
    /// The height of the underlying buffer.
    height: usize,
    // Only applications opting in to tiled, premultiplied presentation keep a frame.
    tiled: Option<Tiles>,
    window_handle: W,
    _display: PhantomData<D>,
}

impl<D, W> Drop for CGImpl<D, W> {
    fn drop(&mut self) {
        // SAFETY: Registered in `new`, must be removed before the observer is deallocated.
        unsafe {
            self.root_layer
                .removeObserver_forKeyPath(&self.observer, ns_string!("contentsScale"));
            self.root_layer
                .removeObserver_forKeyPath(&self.observer, ns_string!("bounds"));
        }
        if self.tiled.is_some() {
            self.layer.removeFromSuperlayer();
        }
    }
}

impl<D: HasDisplayHandle, W: HasWindowHandle> SurfaceInterface<D, W> for CGImpl<D, W> {
    type Context = D;
    type Buffer<'a>
        = BufferImpl<'a, D, W>
    where
        Self: 'a;

    fn new(window_src: W, _display: &D) -> Result<Self, InitError<W>> {
        // `NSView`/`UIView` can only be accessed from the main thread.
        let _mtm = MainThreadMarker::new().ok_or(SoftBufferError::PlatformError(
            Some("can only access Core Graphics handles from the main thread".to_string()),
            None,
        ))?;

        let (root_layer, flipped) = match window_src.window_handle()?.as_raw() {
            RawWindowHandle::AppKit(handle) => {
                // SAFETY: The pointer came from `WindowHandle`, which ensures that the
                // `AppKitWindowHandle` contains a valid pointer to an `NSView`.
                //
                // We use `NSObject` here to avoid importing `objc2-app-kit`.
                let view: &NSObject = unsafe { handle.ns_view.cast().as_ref() };

                #[cfg(target_os = "macos")]
                if PREMULTIPLIED_LAYER.get().is_some() {
                    // Compose the player's SDR tiles in sRGB. Otherwise AppKit can
                    // convert every new tile to the display's ICC profile on the CPU.
                    // SAFETY: this is an NSView on the main thread. Its optional
                    // window accepts the NSColorSpace returned by +sRGBColorSpace.
                    unsafe {
                        let window: Option<Retained<NSObject>> = msg_send![view, window];
                        if let Some(window) = window {
                            let srgb: Retained<NSObject> = msg_send![objc2::class!(NSColorSpace), sRGBColorSpace];
                            let _: () = msg_send![&*window, setColorSpace: &*srgb];
                        }
                    }
                }

                // Force the view to become layer backed
                let _: () = unsafe { msg_send![view, setWantsLayer: Bool::YES] };

                // SAFETY: `-[NSView layer]` returns an optional `CALayer`
                let layer: Option<Retained<CALayer>> = unsafe { msg_send![view, layer] };
                // SAFETY: The window handle is an NSView. Its backing layer inherits its
                // coordinate system, so flip only a bottom-up view for the top-down tiles.
                let flipped: Bool = unsafe { msg_send![view, isFlipped] };
                (
                    layer.expect("failed making the view layer-backed"),
                    !flipped.as_bool(),
                )
            }
            RawWindowHandle::UiKit(handle) => {
                // SAFETY: The pointer came from `WindowHandle`, which ensures that the
                // `UiKitWindowHandle` contains a valid pointer to an `UIView`.
                //
                // We use `NSObject` here to avoid importing `objc2-ui-kit`.
                let view: &NSObject = unsafe { handle.ui_view.cast().as_ref() };

                // SAFETY: `-[UIView layer]` returns `CALayer`
                let layer: Retained<CALayer> = unsafe { msg_send![view, layer] };
                (layer, false)
            }
            _ => return Err(InitError::Unsupported(window_src)),
        };

        // Add a sublayer, to avoid interfering with the root layer, since setting the contents of
        // e.g. a view-controlled layer is brittle.
        let layer = CALayer::new();
        let tiled = PREMULTIPLIED_LAYER.get().map(|name| {
            layer.setName(Some(&NSString::from_str(name)));
            Tiles::default()
        });
        root_layer.addSublayer(&layer);

        // Set the anchor point and geometry. Softbuffer's uses a coordinate system with the origin
        // in the top-left corner.
        //
        // NOTE: This doesn't really matter unless we start modifying the `position` of our layer
        // ourselves, but it's nice to have in place.
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        layer.setGeometryFlipped(tiled.is_none() || flipped);

        // Do not use auto-resizing mask.
        //
        // This is done to work around a bug in macOS 14 and above, where views using auto layout
        // may end up setting fractional values as the bounds, and that in turn doesn't propagate
        // properly through the auto-resizing mask and with contents gravity.
        //
        // Instead, we keep the bounds of the layer in sync with the root layer using an observer,
        // see below.
        //
        // layer.setAutoresizingMask(kCALayerHeightSizable | kCALayerWidthSizable);

        let observer = Observer::new(&layer);
        // Observe changes to the root layer's bounds and scale factor, and apply them to our layer.
        //
        // The previous implementation updated the scale factor inside `resize`, but this works
        // poorly with transactions, and is generally inefficient. Instead, we update the scale
        // factor only when needed because the super layer's scale factor changed.
        //
        // Note that inherent in this is an explicit design decision: We control the `bounds` and
        // `contentsScale` of the layer directly, and instead let the `resize` call that the user
        // controls only be the size of the underlying buffer.
        //
        // SAFETY: Observer deregistered in `Drop` before the observer object is deallocated.
        unsafe {
            root_layer.addObserver_forKeyPath_options_context(
                &observer,
                ns_string!("contentsScale"),
                NSKeyValueObservingOptions::New | NSKeyValueObservingOptions::Initial,
                ptr::null_mut(),
            );
            root_layer.addObserver_forKeyPath_options_context(
                &observer,
                ns_string!("bounds"),
                NSKeyValueObservingOptions::New | NSKeyValueObservingOptions::Initial,
                ptr::null_mut(),
            );
        }

        // Set the content so that it is placed in the top-left corner if it does not have the same
        // size as the surface itself.
        //
        // TODO(madsmtm): Consider changing this to `kCAGravityResize` to stretch the content if
        // resized to something that doesn't fit, see #177.
        layer.setContentsGravity(unsafe { kCAGravityTopLeft });

        // Initialize color space here, to reduce work later on.
        let color_space = CGColorSpace::new_device_rgb().unwrap();

        // Grab initial width and height from the layer (whose properties have just been initialized
        // by the observer using `NSKeyValueObservingOptionInitial`).
        let size = layer.bounds().size;
        let scale_factor = layer.contentsScale();
        let width = (size.width * scale_factor) as usize;
        let height = (size.height * scale_factor) as usize;

        Ok(Self {
            layer: SendCALayer(layer),
            root_layer: SendCALayer(root_layer),
            observer,
            color_space,
            width,
            height,
            _display: PhantomData,
            tiled,
            window_handle: window_src,
        })
    }

    #[inline]
    fn window(&self) -> &W {
        &self.window_handle
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), SoftBufferError> {
        if (self.width, self.height) != (width.get() as usize, height.get() as usize) {
            if let Some(tiled) = &mut self.tiled {
                tiled.frame = None;
                tiled.scale = 0.0;
            }
        }
        self.width = width.get() as usize;
        self.height = height.get() as usize;
        Ok(())
    }

    fn buffer_mut(&mut self) -> Result<BufferImpl<'_, D, W>, SoftBufferError> {
        // A dropped mutable frame may have been modified. The last published tiles
        // stay immutable, and the renderer gets age zero when it next asks for a frame.
        let previous = self.tiled.as_mut().and_then(|tiled| tiled.frame.take());
        let age = u8::from(previous.is_some());
        let buffer =
            previous.unwrap_or_else(|| util::PixelBuffer(vec![0; self.width * self.height]));
        Ok(BufferImpl {
            imp: self,
            buffer,
            age,
        })
    }
}

#[derive(Debug)]
pub struct BufferImpl<'a, D, W> {
    imp: &'a mut CGImpl<D, W>,
    buffer: util::PixelBuffer,
    age: u8,
}

impl<D: HasDisplayHandle, W: HasWindowHandle> BufferInterface for BufferImpl<'_, D, W> {
    fn width(&self) -> NonZeroU32 {
        NonZeroU32::new(self.imp.width as u32).unwrap()
    }

    fn height(&self) -> NonZeroU32 {
        NonZeroU32::new(self.imp.height as u32).unwrap()
    }

    #[inline]
    fn pixels(&self) -> &[u32] {
        &self.buffer
    }

    #[inline]
    fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.buffer
    }

    fn age(&self) -> u8 {
        self.age
    }

    fn present(self) -> Result<(), SoftBufferError> {
        self.present_with_damage(&[])
    }

    fn present_with_damage(self, damage: &[Rect]) -> Result<(), SoftBufferError> {
        let surface = self.imp;
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        if let Some(tiled) = &mut surface.tiled {
            tiled.present(
                self.buffer,
                (surface.width, surface.height),
                &surface.layer,
                &surface.color_space,
                self.age,
                damage,
            );
        } else {
            // Original XRGB presentation on UIKit and ordinary softbuffer surfaces.
            let image = image_from_pixels(
                Arc::new(self.buffer.0),
                surface.width,
                surface.height,
                &surface.color_space,
                CGImageAlphaInfo::NoneSkipFirst,
            );
            // SAFETY: CGImage is a supported type for CALayer contents.
            unsafe { surface.layer.setContents(Some(image.as_ref())) };
        }
        CATransaction::commit();
        Ok(())
    }
}

#[derive(Debug)]
struct Tile {
    layer: SendCALayer,
    pixels: Option<Arc<Vec<u32>>>,
}

#[derive(Debug, Default)]
struct Tiles {
    frame: Option<util::PixelBuffer>,
    layers: Vec<Tile>,
    scale: CGFloat,
}

impl Tiles {
    fn present(
        &mut self,
        frame: util::PixelBuffer,
        size: (usize, usize),
        layer: &CALayer,
        color_space: &CGColorSpace,
        age: u8,
        damage: &[Rect],
    ) {
        // Keep unchanged images in the compositor. In a Retina window a seek
        // marker moves a few pixels in a frame of more than five million pixels.
        const TILE: usize = 256;
        let (width, height) = size;
        let scale = layer.contentsScale();
        let recreate = self.scale != scale;
        if recreate {
            for tile in self.layers.drain(..) {
                tile.layer.removeFromSuperlayer();
            }
            self.scale = scale;
        }
        let full = recreate || age == 0 || damage.is_empty();
        let columns = width.div_ceil(TILE);
        for y in (0..height).step_by(TILE) {
            for x in (0..width).step_by(TILE) {
                let tile_width = TILE.min(width - x);
                let tile_height = TILE.min(height - y);
                if !full
                    && !damage.iter().any(|rect| {
                        let (left, top) = (rect.x as usize, rect.y as usize);
                        left < x + tile_width
                            && top < y + tile_height
                            && x < left.saturating_add(rect.width.get() as usize)
                            && y < top.saturating_add(rect.height.get() as usize)
                    })
                {
                    continue;
                }
                if recreate {
                    let tile = CALayer::new();
                    tile.setContentsScale(scale);
                    tile.setFrame(CGRect::new(
                        CGPoint::new(x as CGFloat / scale, y as CGFloat / scale),
                        CGSize::new(
                            tile_width as CGFloat / scale,
                            tile_height as CGFloat / scale,
                        ),
                    ));
                    layer.addSublayer(&tile);
                    self.layers.push(Tile {
                        layer: SendCALayer(tile),
                        pixels: None,
                    });
                }
                let rows = &frame[y * width + x..];
                let tile = &mut self.layers[(y / TILE) * columns + x / TILE];
                // The dirty rectangle for a changing seek bar includes its entire
                // filled span. Only publish tiles whose pixels actually changed.
                if tile.pixels.as_ref().is_some_and(|previous| {
                    rows.chunks(width)
                        .zip(previous.chunks_exact(tile_width))
                        .all(|(row, old)| &row[..tile_width] == old)
                }) {
                    continue;
                }
                let mut pixels = Vec::with_capacity(tile_width * tile_height);
                for row in rows.chunks(width).take(tile_height) {
                    pixels.extend_from_slice(&row[..tile_width]);
                }
                let pixels = Arc::new(pixels);
                let image = image_from_pixels(
                    Arc::clone(&pixels),
                    tile_width,
                    tile_height,
                    color_space,
                    CGImageAlphaInfo::PremultipliedFirst,
                );
                // SAFETY: CGImage is a supported type for CALayer contents.
                unsafe { tile.layer.setContents(Some(image.as_ref())) };
                tile.pixels = Some(pixels);
            }
        }
        self.frame = Some(frame);
    }
}

fn image_from_pixels(
    pixels: Arc<Vec<u32>>,
    width: usize,
    height: usize,
    color_space: &CGColorSpace,
    alpha: CGImageAlphaInfo,
) -> CFRetained<CGImage> {
    unsafe extern "C-unwind" fn release(info: *mut c_void, _data: NonNull<c_void>, _size: usize) {
        // SAFETY: the provider owns one Arc reference to this immutable pixel buffer.
        drop(unsafe { Arc::from_raw(info.cast::<Vec<u32>>()) });
    }

    let len = pixels.len() * size_of::<u32>();
    let data = pixels.as_ptr().cast::<c_void>();
    let owner = Arc::into_raw(pixels).cast_mut().cast::<c_void>();
    // SAFETY: the provider keeps the Arc alive until the last CGImage reader releases it.
    let data_provider =
        unsafe { CGDataProvider::with_data(owner, data, len, Some(release)).unwrap() };

    // `CGBitmapInfo` consists of a combination of `CGImageAlphaInfo`, `CGImageComponentInfo`
    // `CGImageByteOrderInfo` and `CGImagePixelFormatInfo` (see e.g. `CGBitmapInfoMake`).
    //
    // TODO: Use `CGBitmapInfo::new` once the next version of objc2-core-graphics is released.
    let bitmap_info = CGBitmapInfo(
        alpha.0
            | CGImageComponentInfo::Integer.0
            | CGImageByteOrderInfo::Order32Little.0
            | CGImagePixelFormatInfo::Packed.0,
    );

    unsafe {
        CGImage::new(
            width,
            height,
            8,
            32,
            width * 4,
            Some(color_space),
            bitmap_info,
            Some(&data_provider),
            ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .unwrap()
}

#[derive(Debug)]
struct SendCALayer(Retained<CALayer>);

// SAFETY: CALayer is dubiously thread safe, like most things in Core Animation.
// But since we make sure to do our changes within a CATransaction, it is
// _probably_ fine for us to use CALayer from different threads.
//
// See also:
// https://developer.apple.com/documentation/quartzcore/catransaction/1448267-lock?language=objc
// https://stackoverflow.com/questions/76250226/how-to-render-content-of-calayer-on-a-background-thread
unsafe impl Send for SendCALayer {}
// SAFETY: Same as above.
unsafe impl Sync for SendCALayer {}

impl Deref for SendCALayer {
    type Target = CALayer;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
