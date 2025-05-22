/**
 * I was drunk when I wrote this.
 */
use anyhow::Result;
use cairo::{Antialias, Context, FontFace, Format, ImageSurface, Pattern, Surface};
use drm::control::ClipRect;
use fonts::FontConfig;
use freetype::Library as FtLibrary;
use input::{
    event::{
        device::DeviceEvent,
        touch::{TouchEvent, TouchEventPosition},
        Event, EventTrait,
    },
    Device as InputDevice, Libinput, LibinputInterface,
};
use libc::{c_char, O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY};
use nix::sys::{
    epoll::{Epoll, EpollCreateFlags, EpollEvent, EpollFlags},
    signal::{SigSet, Signal},
};
use rand::Rng;
use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::{
        fd::AsFd,
        unix::{fs::OpenOptionsExt, io::OwnedFd},
    },
    panic::{self, AssertUnwindSafe},
    path::Path,
    thread,
    time::{Duration, Instant},
};

mod display;
mod fonts;

use display::DrmBackend;

fn try_load_png<R>(mut data: R, icon_size: i32) -> Result<ImageSurface>
where
    R: Read,
{
    let surf = ImageSurface::create_from_png(&mut data)?;
    if surf.height() == icon_size && surf.width() == icon_size {
        return Ok(surf);
    }
    let resized = ImageSurface::create(Format::ARgb32, icon_size, icon_size).unwrap();
    let c = Context::new(&resized).unwrap();
    c.scale(
        icon_size as f64 / surf.width() as f64,
        icon_size as f64 / surf.height() as f64,
    );
    c.set_source_surface(surf, 0.0, 0.0).unwrap();
    c.set_antialias(Antialias::Best);
    c.paint().unwrap();
    Ok(resized)
}

pub struct Scene {
    drawables: Vec<Drawable>,
    fontface: FontFace,
}

#[derive(Debug, Clone)]
pub struct Drawable {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub color: (f64, f64, f64),
    pub surface: Option<ImageSurface>,
    pub needs_redraw: bool,
}

impl Drawable {
    fn new(
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        color: (f64, f64, f64),
        surface: Option<ImageSurface>,
    ) -> Self {
        Self {
            x,
            y,
            width,
            height,
            color,
            needs_redraw: true,
            surface,
        }
    }
}

impl Scene {
    fn new(dino: ImageSurface, cactus: ImageSurface) -> Scene {
        let mut drawables = vec![Drawable::new(
            0.0,
            0.0,
            9.0,
            9.0,
            (1.0, 1.0, 1.0),
            Some(dino),
        )];

        let w = cactus.width() as f64;
        let h = cactus.height() as f64;
        let value = Some(cactus);
        for _i in 0..20 {
            drawables.push(Drawable {
                x: -w,
                y: 0.0,
                width: w,
                height: h,
                color: (0.0, 1.0, 0.0),
                needs_redraw: false,
                surface: value.clone(),
            });
        }

        let fc = FontConfig::new();
        let mut pt = fonts::Pattern::new("Adwaita Mono");
        fc.perform_substitutions(&mut pt);
        let pat_match = match fc.match_pattern(&pt) {
        Ok(pat) => pat,
        Err(_) => panic!("Unable to find specified font. If you are using the default config, make sure you have at least one font installed")
    };
        let file_name = pat_match.get_file_name();
        let file_idx = pat_match.get_font_index();
        let ft_library = FtLibrary::init().unwrap();
        let face = ft_library.new_face(file_name, file_idx).unwrap();
        let fontface = FontFace::create_from_ft(&face).unwrap();

        Scene {
            drawables,
            fontface,
        }
    }

    fn draw(
        &mut self,
        width: i32,
        height: i32,
        surface: &Surface,
        time: &TimeStep,
    ) -> Vec<ClipRect> {
        let c = Context::new(surface).unwrap();
        let modified_regions = Vec::new();
        c.translate(height as f64, 0.0);
        c.rotate((90.0f64).to_radians());

        c.set_source_rgb(0.0, 0.0, 0.0);
        c.paint().unwrap();

        for drawable in self.drawables.iter_mut() {
            let x = drawable.x;
            let y = height as f64 - drawable.y;
            if let Some(surface) = &drawable.surface {
                let y = y - surface.height() as f64;
                c.set_source_surface(surface, x, y).unwrap();
                c.rectangle(x, y, surface.width() as f64, surface.height() as f64);
            } else {
                c.set_source_rgb(drawable.color.0, drawable.color.1, drawable.color.2);
                c.rectangle(x, y - drawable.height, drawable.width, drawable.height);
            }

            c.fill().unwrap();

            drawable.needs_redraw = false;
        }

        let timer_text = format!("{:.1}s", time.start_time.elapsed().as_secs_f64());

        c.set_font_face(&self.fontface);
        c.set_font_size(12.0);

        let extends = c.text_extents(&timer_text).unwrap();
        c.move_to(0.0, extends.height());
        c.set_source_rgb(1.0, 1.0, 1.0);
        c.show_text(&timer_text).unwrap();

        modified_regions
    }
}

struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        let mode = flags & O_ACCMODE;

        OpenOptions::new()
            .custom_flags(flags)
            .read(mode == O_RDONLY || mode == O_RDWR)
            .write(mode == O_WRONLY || mode == O_RDWR)
            .open(path)
            .map(|file| file.into())
            .map_err(|err| err.raw_os_error().unwrap())
    }
    fn close_restricted(&mut self, fd: OwnedFd) {
        _ = File::from(fd);
    }
}

#[derive(Debug)]
pub struct TimeStep {
    last_time: Instant,
    start_time: Instant,
}

impl Default for TimeStep {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeStep {
    pub fn new() -> TimeStep {
        TimeStep {
            last_time: Instant::now(),
            start_time: Instant::now(),
        }
    }

    pub fn delta(&mut self) -> f64 {
        let current_time = Instant::now();
        let delta = current_time.duration_since(self.last_time).as_secs_f64();
        self.last_time = current_time;
        delta
    }
}

fn main() {
    let mut drm = DrmBackend::open_card().unwrap();
    loop {
        let _ = panic::catch_unwind(AssertUnwindSafe(|| real_main(&mut drm)));
    }
}

fn real_main(drm: &mut DrmBackend) {
    let (height, width) = drm.mode().size();
    let (db_width, db_height) = drm.fb_info().unwrap().size();

    let dino_png = include_bytes!("dino.png");
    let dino_surface = try_load_png(&dino_png[..], 40).unwrap();

    let cactus_png = include_bytes!("cactus.png");
    let cactus_surface = try_load_png(&cactus_png[..], 24).unwrap();

    let mut scene = Scene::new(dino_surface, cactus_surface);

    let mut surface =
        ImageSurface::create(Format::ARgb32, db_width as i32, db_height as i32).unwrap();

    let mut input_tb = Libinput::new_with_udev(Interface);
    let mut input_main = Libinput::new_with_udev(Interface);
    input_tb.udev_assign_seat("seat-touchbar").unwrap();
    input_main.udev_assign_seat("seat0").unwrap();
    let epoll = Epoll::new(EpollCreateFlags::empty()).unwrap();
    epoll
        .add(input_main.as_fd(), EpollEvent::new(EpollFlags::EPOLLIN, 0))
        .unwrap();
    epoll
        .add(input_tb.as_fd(), EpollEvent::new(EpollFlags::EPOLLIN, 1))
        .unwrap();
    let mut dev_name_c = [0 as c_char; 80];
    let dev_name = "Dynamic Function Row Virtual Input Device".as_bytes();
    for i in 0..dev_name.len() {
        dev_name_c[i] = dev_name[i] as c_char;
    }

    let mut digitizer: Option<InputDevice> = None;
    let mut base_time = TimeStep::new();

    let mut dino_velocity: f64 = 0.0;
    let da_dino_velocity = &mut dino_velocity as *mut f64;

    let dino = scene.drawables.as_mut_ptr_range().start;
    let da_dino_too = unsafe { &mut *dino } as *mut Drawable;
    let tree_num = scene.drawables.len() - 1;
    let mut rng = rand::thread_rng();

    let mut input_down_time: Option<Instant> = None;
    let max_down_time = 120;

    let trees = unsafe { scene.drawables.as_mut_ptr().add(1) };

    let player_x_offset = 10.0;

    let jump = |elapsed: u128| unsafe {
        if (*da_dino_too).y == 0.0 {
            let clamped_elapsed = elapsed.clamp(50, max_down_time) as f64 / max_down_time as f64;
            (*da_dino_velocity) += 400.0 * clamped_elapsed.powf(1f64 / 2f64);
            (*da_dino_too).y += 1.0;
        }
    };

    unsafe {
        (*dino).x = player_x_offset;
    }

    loop {
        let delta = base_time.delta();
        let game_time = base_time.start_time.elapsed().as_secs_f64();

        unsafe {
            dino_velocity -= 30.0 * delta * (*dino).y;

            (*dino).y += dino_velocity * delta;
            if (*dino).y <= 0.0 {
                (*dino).y = 0.0;
                dino_velocity = 0.0;
            }
            (*dino).needs_redraw = true;
            
            let mut offset: f64 = 0.0;

            for tree_index in 0..tree_num {
                let da_tree = trees.add(tree_index);
                (*da_tree).x -= 150.0 * delta * game_time.powf(1f64 / 7f64);

                if (*da_tree).x + (*da_tree).width <= 0.0 {
                    // reset tree
                    (*da_tree).x = width as f64 + offset;
                    offset += rng.gen_range(150.0..500.0);
                    continue;
                }

                if (*da_tree).x <= (*dino).width + player_x_offset
                    && (*da_tree).x > player_x_offset
                    && (*dino).y <= (*da_tree).height
                {
                    // gameover
                    return ();
                }
            }
        }

        scene.draw(width as i32, height as i32, &surface, &base_time);
        let data = surface.data().unwrap();
        drm.map().unwrap().as_mut()[..data.len()].copy_from_slice(&data);
        drm.dirty(&[ClipRect::new(0, 0, height as u16, width as u16)])
            .unwrap();

        if let Some(down_time) = input_down_time {
            if down_time.elapsed().as_millis() >= max_down_time {
                input_down_time = None;
                let elapsed = down_time.elapsed().as_millis();
                (jump)(elapsed);
            }
        }

        input_tb.dispatch().unwrap();
        input_main.dispatch().unwrap();
        for event in &mut input_tb.clone().chain(input_main.clone()) {
            match event {
                Event::Device(DeviceEvent::Added(evt)) => {
                    let dev = evt.device();
                    if dev.name().contains(" Touch Bar") {
                        digitizer = Some(dev);
                    }
                }
                Event::Touch(te) => {
                    if Some(te.device()) != digitizer {
                        continue;
                    }
                    match te {
                        TouchEvent::Down(_dn) => {
                            input_down_time = Some(Instant::now());
                        }
                        TouchEvent::Motion(_mtn) => {
                            if input_down_time.is_none() {
                                input_down_time = Some(Instant::now());
                            }
                        }
                        TouchEvent::Up(_up) => {
                            if let Some(down_time) = input_down_time {
                                input_down_time = None;
                                let elapsed = down_time.elapsed().as_millis();
                                (jump)(elapsed);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        let sleep_time = (1 / 144) as f64 - delta;
        if sleep_time > 0.0 {
            thread::sleep(Duration::from_secs(sleep_time as u64));
        }
    }
}
