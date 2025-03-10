use self::timer_slack::*;
use super::*;
use async_rt::wait::Waiter;
use core::convert::TryFrom;
use process::pid_t;
use rcore_fs::dev::TimeProvider;
use rcore_fs::vfs::Timespec;
use std::time::Duration;
use std::{fmt, u64};
pub use vdso_time::ClockId;

mod syscalls;
pub mod timer_file;
pub mod timer_slack;
pub mod up_time;

pub use self::syscalls::*;
pub use timer_file::{TimerCreationFlags, TimerFile, TimerSetFlags};
pub use timer_slack::TIMERSLACK;

#[allow(non_camel_case_types)]
pub type time_t = i64;

#[allow(non_camel_case_types)]
pub type suseconds_t = i64;

#[allow(non_camel_case_types)]
pub type clock_t = i64;

/// Clock ticks per second
pub const SC_CLK_TCK: u64 = 100;

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
#[allow(non_camel_case_types)]
pub struct timeval_t {
    sec: time_t,
    usec: suseconds_t,
}

impl timeval_t {
    pub fn new(sec: time_t, usec: suseconds_t) -> Self {
        let time = Self { sec, usec };

        time.validate().unwrap();
        time
    }

    pub fn validate(&self) -> Result<()> {
        if self.sec >= 0 && self.usec >= 0 && self.usec < 1_000_000 {
            Ok(())
        } else {
            return_errno!(EINVAL, "invalid value for timeval_t");
        }
    }

    pub fn sec(&self) -> time_t {
        self.sec
    }

    pub fn usec(&self) -> suseconds_t {
        self.usec
    }

    pub fn as_duration(&self) -> Duration {
        Duration::new(self.sec as u64, (self.usec * 1_000) as u32)
    }
}

impl From<Duration> for timeval_t {
    fn from(duration: Duration) -> timeval_t {
        let sec = duration.as_secs() as time_t;
        let usec = duration.subsec_micros() as i64;
        debug_assert!(sec >= 0); // nsec >= 0 always holds
        timeval_t { sec, usec }
    }
}

pub fn do_gettimeofday() -> timeval_t {
    let tv = timeval_t::from(vdso_time::clock_gettime(ClockId::CLOCK_REALTIME).unwrap());
    tv.validate()
        .expect("gettimeofday returned invalid timeval_t");
    tv
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
#[allow(non_camel_case_types)]
pub struct timespec_t {
    sec: time_t,
    nsec: i64,
}

impl From<Duration> for timespec_t {
    fn from(duration: Duration) -> timespec_t {
        let sec = duration.as_secs() as time_t;
        let nsec = duration.subsec_nanos() as i64;
        debug_assert!(sec >= 0); // nsec >= 0 always holds
        timespec_t { sec, nsec }
    }
}

impl From<timeval_t> for timespec_t {
    fn from(timval: timeval_t) -> timespec_t {
        timespec_t {
            sec: timval.sec(),
            nsec: timval.usec() * 1_000,
        }
    }
}

impl From<time_t> for timespec_t {
    fn from(time: time_t) -> timespec_t {
        timespec_t { sec: time, nsec: 0 }
    }
}

impl timespec_t {
    pub fn from_raw_ptr(ptr: *const timespec_t) -> Result<timespec_t> {
        let ts = unsafe { *ptr };
        ts.validate()?;
        Ok(ts)
    }

    pub fn validate(&self) -> Result<()> {
        if self.sec >= 0 && self.nsec >= 0 && self.nsec < 1_000_000_000 {
            Ok(())
        } else {
            return_errno!(EINVAL, "invalid value for timespec_t");
        }
    }

    pub fn sec(&self) -> time_t {
        self.sec
    }

    pub fn nsec(&self) -> i64 {
        self.nsec
    }

    pub fn as_duration(&self) -> Duration {
        Duration::new(self.sec as u64, self.nsec as u32)
    }
}

#[allow(non_camel_case_types)]
pub type clockid_t = i32;

pub fn do_clock_gettime(clockid: ClockId) -> Result<timespec_t> {
    // TODO: support CLOCK_PROCESS_CPUTIME_ID and CLOCK_THREAD_CPUTIME_ID.
    if clockid == ClockId::CLOCK_PROCESS_CPUTIME_ID || clockid == ClockId::CLOCK_THREAD_CPUTIME_ID {
        return_errno!(
            EINVAL,
            "Not support CLOCK_PROCESS_CPUTIME_ID or CLOCK_THREAD_CPUTIME_ID"
        );
    }
    let tv = timespec_t::from(vdso_time::clock_gettime(clockid).unwrap());
    tv.validate()
        .expect("clock_gettime returned invalid timespec");
    Ok(tv)
}

pub fn do_clock_getres(clockid: ClockId) -> Result<timespec_t> {
    let res = timespec_t::from(vdso_time::clock_getres(clockid).unwrap());
    let validate_resolution = |res: &timespec_t| -> Result<()> {
        // The resolution can be ranged from 1 nanosecond to a few milliseconds
        if res.sec == 0 && res.nsec > 0 && res.nsec < 1_000_000_000 {
            Ok(())
        } else {
            return_errno!(EINVAL, "invalid value for resolution");
        }
    };
    // do sanity check
    validate_resolution(&res).expect("clock_getres returned invalid resolution");
    Ok(res)
}

const TIMER_ABSTIME: i32 = 0x01;

pub async fn do_nanosleep(req: &timespec_t, rem: Option<&mut timespec_t>) -> Result<isize> {
    do_clock_nanosleep(ClockId::CLOCK_REALTIME, 0, req, rem).await
}

pub async fn do_clock_nanosleep(
    clockid: ClockId,
    flags: i32,
    req: &timespec_t,
    rem: Option<&mut timespec_t>,
) -> Result<isize> {
    match clockid {
        ClockId::CLOCK_REALTIME | ClockId::CLOCK_MONOTONIC | ClockId::CLOCK_BOOTTIME => {}
        _ => {
            // Follow the implementaion in Linux here
            return_errno!(EINVAL, "clockid was invalid");
        }
    }

    let waiter = Waiter::new();
    let mut duration = {
        if flags != TIMER_ABSTIME {
            req.as_duration()
        } else {
            let now = vdso_time::clock_gettime(clockid)?;
            let target = req.as_duration();
            if target > now {
                target - now
            } else {
                return Ok(0);
            }
        }
    };

    let wait_res = waiter.wait_timeout(Some(&mut duration)).await;
    let res = match wait_res {
        Ok(_) => unreachable!("this waiter has been unexpected interrupted"),
        Err(e) => {
            if e.errno() == ETIMEDOUT {
                Ok(0)
            } else if e.errno() == EINTR {
                Err(errno!(EINTR, "sleep was interrupted by a signal handler"))
            } else {
                unreachable!("unexpected errno from wait_timeout()");
            }
        }
    };

    if flags != TIMER_ABSTIME {
        if let Some(rem) = rem {
            *rem = timespec_t {
                sec: duration.as_secs() as i64,
                nsec: duration.subsec_nanos() as i64,
            };
        }
    }

    res
}

pub fn do_thread_getcpuclock() -> Result<timespec_t> {
    extern "C" {
        fn occlum_ocall_thread_getcpuclock(ret: *mut c_int, tp: *mut timespec_t) -> sgx_status_t;
    }

    let mut tv: timespec_t = Default::default();
    try_libc!({
        let mut retval: i32 = 0;
        let status = occlum_ocall_thread_getcpuclock(&mut retval, &mut tv as *mut timespec_t);
        assert!(status == sgx_status_t::SGX_SUCCESS);
        retval
    });
    tv.validate()?;
    Ok(tv)
}

pub fn do_rdtsc() -> (u32, u32) {
    extern "C" {
        fn occlum_ocall_rdtsc(low: *mut u32, high: *mut u32) -> sgx_status_t;
    }
    let mut low = 0;
    let mut high = 0;
    let sgx_status = unsafe { occlum_ocall_rdtsc(&mut low, &mut high) };
    assert!(sgx_status == sgx_status_t::SGX_SUCCESS);
    (low, high)
}

// For SEFS
pub struct OcclumTimeProvider;

impl TimeProvider for OcclumTimeProvider {
    fn current_time(&self) -> Timespec {
        let time = do_clock_gettime(ClockId::CLOCK_REALTIME).expect("do_clock_gettime() failed");
        Timespec {
            sec: time.sec,
            nsec: time.nsec,
        }
    }
}

// For Timerfd
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
#[allow(non_camel_case_types)]
pub struct itimerspec_t {
    it_interval: timespec_t,
    it_value: timespec_t,
}

#[derive(Debug, Default)]
pub struct TimerfileDurations {
    it_interval: Duration,
    it_value: Duration,
}

impl itimerspec_t {
    pub fn from_raw_ptr(ptr: *const itimerspec_t) -> Result<itimerspec_t> {
        let its = unsafe { *ptr };
        its.validate()?;
        Ok(its)
    }
    pub fn validate(&self) -> Result<()> {
        self.it_interval.validate()?;
        self.it_value.validate()?;
        Ok(())
    }
}

impl From<TimerfileDurations> for itimerspec_t {
    fn from(durations: TimerfileDurations) -> itimerspec_t {
        let it_interval: timespec_t = durations.it_interval.into();
        let it_value: timespec_t = durations.it_value.into();

        itimerspec_t {
            it_interval,
            it_value,
        }
    }
}

impl From<itimerspec_t> for TimerfileDurations {
    fn from(itime: itimerspec_t) -> TimerfileDurations {
        let it_interval = itime.it_interval.as_duration();
        let it_value = itime.it_value.as_duration();

        TimerfileDurations {
            it_interval,
            it_value,
        }
    }
}
