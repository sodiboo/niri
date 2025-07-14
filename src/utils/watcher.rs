//! File modification watcher.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, SystemTime};
use std::{io, thread};

use niri_config::ConfigPath;
use smithay::reexports::calloop::channel::SyncSender;

pub struct Watcher {
    should_stop: Arc<AtomicBool>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.should_stop.store(true, Ordering::SeqCst);
    }
}

impl Watcher {
    pub fn new<T: Send + 'static>(
        path: ConfigPath,
        process: impl FnMut(&ConfigPath) -> T + Send + 'static,
        changed: SyncSender<T>,
    ) -> Self {
        Self::with_start_notification(path, process, changed, None)
    }

    pub fn with_start_notification<T: Send + 'static>(
        config_path: ConfigPath,
        mut process: impl FnMut(&ConfigPath) -> T + Send + 'static,
        changed: SyncSender<T>,
        started: Option<mpsc::SyncSender<()>>,
    ) -> Self {
        let should_stop = Arc::new(AtomicBool::new(false));

        {
            let should_stop = should_stop.clone();
            thread::Builder::new()
                .name(format!("Filesystem Watcher for {config_path:?}"))
                .spawn(move || {
                    // this "should" be as simple as storing the last seen mtime,
                    // and if the contents change without updating mtime, we ignore it.
                    //
                    // but that breaks if the config is a symlink, and its target
                    // changes but the new target and old target have identical mtimes.
                    // in which case we should *not* ignore it; this is an entirely different file.
                    //
                    // in practice, this edge case does not occur on systems other than nix.
                    // because, on nix, everything is a symlink to /nix/store
                    // and /nix/store keeps no mtime (= 1970-01-01)
                    // so, symlink targets change frequently when mtime doesn't.
                    //
                    // therefore, we must also store the canonical path, along with its mtime

                    fn see_path(path: &Path) -> io::Result<(SystemTime, PathBuf)> {
                        let canon = path.canonicalize()?;
                        let mtime = canon.metadata()?.modified()?;
                        Ok((mtime, canon))
                    }

                    fn see(config_path: &ConfigPath) -> io::Result<(SystemTime, PathBuf)> {
                        match config_path {
                            ConfigPath::Explicit(path) => see_path(path),
                            ConfigPath::Regular {
                                user_path,
                                system_path,
                            } => see_path(user_path).or_else(|_| see_path(system_path)),
                        }
                    }

                    let mut last_props = see(&config_path).ok();

                    if let Some(started) = started {
                        let _ = started.send(());
                    }

                    loop {
                        thread::sleep(Duration::from_millis(500));

                        if should_stop.load(Ordering::SeqCst) {
                            break;
                        }

                        if let Ok(new_props) = see(&config_path) {
                            if last_props.as_ref() != Some(&new_props) {
                                trace!("config file changed.");

                                let rv = process(&config_path);

                                if let Err(err) = changed.send(rv) {
                                    warn!("error sending change notification: {err:?}");
                                    break;
                                }

                                last_props = Some(new_props);
                            }
                        }
                    }

                    debug!("exiting watcher thread for {config_path:?}");
                })
                .unwrap();
        }

        Self { should_stop }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs::{self, File, FileTimes};
    use std::io::Write;
    use std::sync::atomic::AtomicU8;

    use calloop::channel::{sync_channel, Event};
    use calloop::EventLoop;
    use smithay::reexports::rustix::fs::{futimens, Timestamps};
    use smithay::reexports::rustix::time::Timespec;
    use xshell::{cmd, Shell, TempDir};

    use super::*;

    type Result<T = (), E = Box<dyn Error>> = std::result::Result<T, E>;

    fn canon(config_path: &ConfigPath) -> &PathBuf {
        match config_path {
            ConfigPath::Explicit(path) => path,
            ConfigPath::Regular {
                user_path,
                system_path,
            } => {
                if user_path.exists() {
                    user_path
                } else {
                    system_path
                }
            }
        }
    }

    enum TestPath<P> {
        Explicit(P),
        Regular { user_path: P, system_path: P },
    }

    impl<P: AsRef<Path>> TestPath<P> {
        fn as_ref(&self) -> TestPath<&Path> {
            match self {
                TestPath::Explicit(path) => TestPath::Explicit(path.as_ref()),
                TestPath::Regular {
                    user_path,
                    system_path,
                } => TestPath::Regular {
                    user_path: user_path.as_ref(),
                    system_path: system_path.as_ref(),
                },
            }
        }

        fn no_setup<'a>(
            &'a self,
        ) -> Test<
            'a,
            impl FnOnce(&Shell) -> Result,
            impl FnOnce(&ConfigPath),
            impl FnOnce(&Shell, &mut TestUtil) -> Result,
        > {
            self.setup_any(|_| Ok(())).assert_initial_not_exists()
        }

        fn setup<'a, Discard>(
            &'a self,
            setup: impl FnOnce(&Shell) -> xshell::Result<Discard>,
        ) -> TestSetup<'a, impl FnOnce(&Shell) -> Result> {
            self.setup_any(|sh| {
                _ = setup(sh)?;
                Ok(())
            })
        }

        fn setup_any<'a, Setup>(&'a self, setup: Setup) -> TestSetup<'a, Setup>
        where
            Setup: FnOnce(&Shell) -> Result,
        {
            TestSetup {
                path: self.as_ref(),
                setup,
            }
        }

        fn _setup_any(
            self,
            setup: impl FnOnce(&Shell) -> Result,
        ) -> Result<(Shell, TempDir, ConfigPath)> {
            let sh = Shell::new()?;
            let temp_dir = sh.create_temp_dir()?;
            sh.change_dir(temp_dir.path());

            let config_path = match self {
                TestPath::Explicit(path) => ConfigPath::Explicit(sh.current_dir().join(path)),
                TestPath::Regular {
                    user_path,
                    system_path,
                } => ConfigPath::Regular {
                    user_path: sh.current_dir().join(user_path),
                    system_path: sh.current_dir().join(system_path),
                },
            };

            setup(&sh)?;

            Ok((sh, temp_dir, config_path))
        }
    }

    struct TestSetup<'a, Setup> {
        path: TestPath<&'a Path>,
        setup: Setup,
    }

    fn empty_body(_: &Shell, _: &mut TestUtil) -> Result {
        Ok(())
    }

    impl<'a, Setup> TestSetup<'a, Setup> {
        fn assert_initial_not_exists(
            self,
        ) -> Test<'a, Setup, impl FnOnce(&ConfigPath), impl FnOnce(&Shell, &mut TestUtil) -> Result>
        {
            let Self { path, setup } = self;

            Test {
                path,
                setup,
                check: |config_path: &ConfigPath| {
                    let canon = canon(config_path);
                    assert!(!canon.exists(), "initial should not exist at {canon:?}");
                },
                body: empty_body,
            }
        }
        fn assert_initial(
            self,
            expected: impl Into<String>,
        ) -> Test<'a, Setup, impl FnOnce(&ConfigPath), impl FnOnce(&Shell, &mut TestUtil) -> Result>
        {
            let expected = expected.into();
            let Self { path, setup } = self;
            Test {
                path,
                setup,
                check: move |config_path: &ConfigPath| {
                    let canon = canon(&config_path);
                    assert!(canon.exists(), "initial should exist at {canon:?}");
                    let actual = fs::read_to_string(canon).unwrap();
                    assert_eq!(actual, expected, "initial file contents do not match");
                },
                body: empty_body,
            }
        }
    }

    struct Test<'a, Setup, Check, Body> {
        path: TestPath<&'a Path>,
        setup: Setup,
        check: Check,
        body: Body,
    }

    impl<'a, Setup, Check, Body> Test<'a, Setup, Check, Body> {
        fn change<Change, Discard>(
            self,
            change: Change,
        ) -> TestChange<'a, Setup, Check, Body, impl FnOnce(&Shell) -> Result>
        where
            Change: FnOnce(&Shell) -> xshell::Result<Discard>,
        {
            self.change_any(|sh| {
                change(sh)?;
                Ok(())
            })
        }

        fn change_any<Change>(self, change: Change) -> TestChange<'a, Setup, Check, Body, Change>
        where
            Change: FnOnce(&Shell) -> Result,
        {
            let Self {
                path,
                setup,
                check,
                body,
            } = self;
            TestChange {
                path,
                setup,
                check,
                body,
                change,
            }
        }
    }

    struct TestChange<'a, Setup, Check, Body, Change> {
        path: TestPath<&'a Path>,
        setup: Setup,
        check: Check,
        body: Body,
        change: Change,
    }

    impl<'a, Setup, Check, Body, Change> TestChange<'a, Setup, Check, Body, Change>
    where
        Body: FnOnce(&Shell, &mut TestUtil) -> Result,
        Change: FnOnce(&Shell) -> Result,
    {
        fn assert_unchanged(
            self,
        ) -> Test<'a, Setup, Check, impl FnOnce(&Shell, &mut TestUtil) -> Result> {
            self.with_assertion(|test| test.assert_unchanged())
        }

        fn assert_changed_to(
            self,
            expected: &'static str,
        ) -> Test<'a, Setup, Check, impl FnOnce(&Shell, &mut TestUtil) -> Result> {
            self.with_assertion(|test| test.assert_changed_to(expected))
        }

        fn with_assertion(
            self,
            assertion: impl FnOnce(&mut TestUtil),
        ) -> Test<'a, Setup, Check, impl FnOnce(&Shell, &mut TestUtil) -> Result> {
            let Self {
                path,
                setup,
                check,
                body: prev,
                change,
            } = self;
            Test {
                path,
                setup,
                check,
                body: move |sh: &Shell, test: &mut TestUtil| {
                    prev(sh, test)?;
                    test.pass_time(); // new mtime before each change
                    change(sh)?;
                    assertion(test);
                    Ok(())
                },
            }
        }
    }

    impl<'a, Setup, Check, Body> Test<'a, Setup, Check, Body>
    where
        Setup: FnOnce(&Shell) -> Result,
        Check: FnOnce(&ConfigPath),
        Body: FnOnce(&Shell, &mut TestUtil) -> Result,
    {
        fn run(self) -> Result {
            let Self {
                path,
                setup,
                check,
                body,
            } = self;

            let sh = Shell::new()?;
            let temp_dir = sh.create_temp_dir()?;
            sh.change_dir(temp_dir.path());

            let config_path = match path {
                TestPath::Explicit(path) => ConfigPath::Explicit(sh.current_dir().join(path)),
                TestPath::Regular {
                    user_path,
                    system_path,
                } => ConfigPath::Regular {
                    user_path: sh.current_dir().join(user_path),
                    system_path: sh.current_dir().join(system_path),
                },
            };

            setup(&sh)?;
            check(&config_path);

            let (tx, rx) = sync_channel(1);
            let (started_tx, started_rx) = mpsc::sync_channel(1);

            let watcher = Watcher::with_start_notification(
                config_path,
                |config_path| canon(config_path).clone(),
                tx,
                Some(started_tx),
            );

            started_rx.recv()?;

            let event_loop = EventLoop::try_new()?;
            event_loop
                .handle()
                .insert_source(rx, |event, (), latest_path| {
                    if let Event::Msg(path) = event {
                        *latest_path = Some(path);
                    }
                })?;

            let mut test = TestUtil {
                event_loop,
                watcher,
            };

            test.assert_unchanged();
            body(&sh, &mut test)?;
            test.assert_unchanged();
            Ok(())
        }
    }

    struct TestUtil<'a> {
        event_loop: EventLoop<'a, Option<PathBuf>>,
        watcher: Watcher,
    }

    impl<'a> TestUtil<'a> {
        fn pass_time(&self) {
            thread::sleep(Duration::from_millis(100));
        }

        fn assert_unchanged(&mut self) {
            let mut new_path = None;
            self.event_loop
                .dispatch(Duration::from_millis(750), &mut new_path)
                .unwrap();
            assert_eq!(
                new_path, None,
                "watcher should not have noticed any changes"
            );
        }

        fn assert_changed_to(&mut self, expected: &str) {
            let mut new_path = None;
            self.event_loop
                .dispatch(Duration::from_millis(750), &mut new_path)
                .unwrap();
            let Some(new_path) = new_path else {
                panic!("watcher should have noticed a change, but it didn't");
            };
            let actual = fs::read_to_string(&new_path).unwrap();
            assert_eq!(actual, expected, "watcher gave the wrong file");
        }
    }

    fn check(
        setup: impl FnOnce(&Shell) -> Result<(), Box<dyn Error>>,
        change: impl FnOnce(&Shell) -> Result<(), Box<dyn Error>>,
    ) {
        let sh = Shell::new().unwrap();
        let temp_dir = sh.create_temp_dir().unwrap();
        sh.change_dir(temp_dir.path());
        // let dir = sh.create_dir("xshell").unwrap();
        // sh.change_dir(dir);

        let mut config_path = sh.current_dir();
        config_path.push("niri");
        config_path.push("config.kdl");

        setup(&sh).unwrap();

        let changed = AtomicU8::new(0);

        let mut event_loop = EventLoop::try_new().unwrap();
        let loop_handle = event_loop.handle();

        let (tx, rx) = sync_channel(1);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let _watcher = Watcher::with_start_notification(
            ConfigPath::Explicit(config_path.clone()),
            |_| (),
            tx,
            Some(started_tx),
        );
        loop_handle
            .insert_source(rx, |_, _, _| {
                changed.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        started_rx.recv().unwrap();

        // HACK: if we don't sleep, files might have the same mtime.
        thread::sleep(Duration::from_millis(100));

        change(&sh).unwrap();

        event_loop
            .dispatch(Duration::from_millis(750), &mut ())
            .unwrap();

        assert_eq!(changed.load(Ordering::SeqCst), 1);

        // Verify that the watcher didn't break.
        sh.write_file(&config_path, "c").unwrap();

        event_loop
            .dispatch(Duration::from_millis(750), &mut ())
            .unwrap();

        assert_eq!(changed.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn change_file() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup(|sh| sh.write_file("niri/config.kdl", "a"))
            .assert_initial("a")
            .change(|sh| sh.write_file("niri/config.kdl", "b"))
            .assert_changed_to("b")
            .run()
    }

    #[test]
    fn overwrite_but_dont_change_file() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup(|sh| sh.write_file("niri/config.kdl", "a"))
            .assert_initial("a")
            .change(|sh| sh.write_file("niri/config.kdl", "a"))
            .assert_changed_to("a")
            .run()
    }

    #[test]
    fn touch_file() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup(|sh| sh.write_file("niri/config.kdl", "a"))
            .assert_initial("a")
            .change(|sh| cmd!(sh, "touch niri/config.kdl").run())
            .assert_changed_to("a")
            .run()
    }

    #[test]
    fn create_file() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup(|sh| sh.create_dir("niri"))
            .assert_initial_not_exists()
            .change(|sh| sh.write_file("niri/config.kdl", "a"))
            .assert_changed_to("a")
            .run()
    }

    #[test]
    fn create_dir_and_file() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .no_setup()
            .change(|sh| sh.write_file("niri/config.kdl", "a"))
            .assert_changed_to("a")
            .run()
    }

    #[test]
    fn change_linked_file() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup(|sh| {
                sh.write_file("niri/config2.kdl", "a")?;
                cmd!(sh, "ln -sf config2.kdl niri/config.kdl").run()
            })
            .assert_initial("a")
            .change(|sh| sh.write_file("niri/config2.kdl", "b"))
            .assert_changed_to("b")
            .run()
    }

    #[test]
    fn change_file_in_linked_dir() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup(|sh| {
                sh.write_file("niri2/config.kdl", "a")?;
                cmd!(sh, "ln -s niri2 niri").run()
            })
            .assert_initial("a")
            .change(|sh| sh.write_file("niri2/config.kdl", "b"))
            .assert_changed_to("b")
            .run()
    }

    #[test]
    fn remove_file() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup(|sh| sh.write_file("niri/config.kdl", "a"))
            .assert_initial("a")
            .change(|sh| sh.remove_path("niri/config.kdl"))
            .assert_unchanged()
            .run()
    }

    #[test]
    fn remove_dir() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup(|sh| sh.write_file("niri/config.kdl", "a"))
            .assert_initial("a")
            .change(|sh| sh.remove_path("niri"))
            .assert_unchanged()
            .run()
    }

    #[test]
    fn recreate_file() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup(|sh| sh.write_file("niri/config.kdl", "a"))
            .assert_initial("a")
            .change(|sh| {
                sh.remove_path("niri/config.kdl")?;
                sh.write_file("niri/config.kdl", "b")
            })
            .assert_changed_to("b")
            .run()
    }

    #[test]
    fn recreate_dir() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup(|sh| {
                sh.write_file("niri/config.kdl", "a")?;
                Ok(())
            })
            .assert_initial("a")
            .change(|sh| {
                sh.remove_path("niri")?;
                sh.write_file("niri/config.kdl", "b")
            })
            .assert_changed_to("b")
            .run()
    }

    #[test]
    fn swap_dir() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup(|sh| sh.write_file("niri/config.kdl", "a"))
            .assert_initial("a")
            .change(|sh| {
                sh.write_file("niri2/config.kdl", "b")?;
                sh.remove_path("niri")?;
                cmd!(sh, "mv niri2 niri").run()
            })
            .assert_changed_to("b")
            .run()
    }

    #[test]
    fn swap_dir_link() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup(|sh| {
                sh.write_file("niri2/config.kdl", "a")?;
                cmd!(sh, "ln -s niri2 niri").run()
            })
            .assert_initial("a")
            .change(|sh| {
                sh.write_file("niri3/config.kdl", "b")?;
                sh.remove_path("niri")?;
                cmd!(sh, "ln -s niri3 niri").run()
            })
            .assert_changed_to("b")
            .run()
    }

    fn create_epoch(path: impl AsRef<Path>, content: &str) -> Result {
        let mut file = File::create(path)?;
        file.write_all(content.as_bytes())?;
        file.set_times(
            FileTimes::new()
                .set_accessed(SystemTime::UNIX_EPOCH)
                .set_modified(SystemTime::UNIX_EPOCH),
        )?;
        file.sync_all()?;
        Ok(())
    }

    #[test]
    fn swap_just_link() -> Result {
        TestPath::Explicit("niri/config.kdl")
            .setup_any(|sh| {
                let dir = sh.current_dir().join("niri");

                sh.create_dir(&dir)?;

                create_epoch(dir.join("config2.kdl"), "a")?;
                create_epoch(dir.join("config3.kdl"), "b")?;

                cmd!(sh, "ln -s config2.kdl niri/config.kdl").run()?;

                Ok(())
            })
            .assert_initial("a")
            .change(|sh| cmd!(sh, "ln -sf config3.kdl niri/config.kdl").run())
            .assert_changed_to("b")
            .run()
    }

    #[test]
    fn swap_many_regular() -> Result {
        TestPath::Regular {
            user_path: "user-niri/config.kdl",
            system_path: "system-niri/config.kdl",
        }
        .setup(|sh| sh.write_file("system-niri/config.kdl", "system config"))
        .assert_initial("system config")
        // .change(|sh| sh.write_file("user-niri/config.kdl", "user config"))
        // .assert_changed_to("user config")
        // .change(|sh| cmd!(sh, "touch system-niri/config.kdl").run())
        // .assert_unchanged()
        // .change(|sh| sh.remove_path("system-niri"))
        // .assert_unchanged()
        // .change(|sh| sh.write_file("system-niri/config.kdl", "new system config"))
        // .assert_unchanged()
        // .change(|sh| sh.remove_path("user-niri"))
        // .assert_changed_to("new system config")
        // .change(|sh| sh.write_file("system-niri/config.kdl", "updated system config"))
        // .assert_changed_to("updated system config")
        // .change(|sh| sh.write_file("user-niri/config.kdl", "new user config"))
        // .assert_changed_to("new user config")
        .run()
    }

    #[test]
    fn swap_many_links_regular_like_nixos() -> Result {
        TestPath::Regular {
            user_path: "user-niri/config.kdl",
            system_path: "system-niri/config.kdl",
        }
        .setup_any(|sh| {
               let store = sh.current_dir().join("store");
 
            sh.create_dir(&store)?;

            create_epoch(store.join("gen1"), "gen 1")?;
            create_epoch(store.join("gen2"), "gen 2")?;
            create_epoch(store.join("gen3"), "gen 3")?;

            sh.create_dir("user-niri")?;
            sh.create_dir("system-niri")?;

            Ok(())
        })
        .assert_initial_not_exists()
        .change(|sh| cmd!(sh, "ln -s $(realpath store)/gen1 user-niri/config.kdl").run())
        .assert_changed_to("gen 1")
        .run()
        // .run(|sh, test| {
        //     cmd!(sh, "ln -s $(realpath store)/gen1 user-niri/config.kdl").run()?;
        //     test.assert_changed_to("gen 1");

        //     cmd!(sh, "ln -s {store}/gen2 system-niri/config.kdl").run()?;
        //     test.assert_unchanged();

        //     cmd!(sh, "unlink user-niri/config.kdl").run()?;
        //     test.assert_changed_to("gen 2");

        //     cmd!(sh, "ln -s {store}/gen3 user-niri/config.kdl").run()?;
        //     test.assert_changed_to("gen 3");

        //     cmd!(sh, "ln -sf {store}/gen1 system-niri/config.kdl").run()?;
        //     test.assert_unchanged();

        //     cmd!(sh, "unlink system-niri/config.kdl").run()?;
        //     test.assert_unchanged();

        //     cmd!(sh, "ln -s {store}/gen1 system-niri/config.kdl").run()?;
        //     test.assert_unchanged();

        //     cmd!(sh, "unlink user-niri/config.kdl").run()?;
        //     test.assert_changed_to("gen 1");

        //     Ok(())
        // })
    }
}
