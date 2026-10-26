//! Windows job objects: a job holds a process and every process it starts,
//! and terminating or dropping the job kills them all. It is the Windows
//! counterpart of a Unix process group that is killed as a whole. Elsewhere
//! this crate is empty.

#[cfg(windows)]
pub use windows::Job;

#[cfg(windows)]
mod windows {
    use std::io;
    use std::os::windows::io::RawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct Job(HANDLE);

    // SAFETY: a job handle may be used and closed from any thread.
    unsafe impl Send for Job {}
    // SAFETY: as above; the methods only pass the handle to thread-safe calls.
    unsafe impl Sync for Job {}

    impl Job {
        pub fn containing(process: RawHandle) -> io::Result<Self> {
            // SAFETY: null attributes and name create an anonymous job.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self(handle);
            // SAFETY: an all-zero limit structure is valid; one flag is set below.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` is the structure this information class expects.
            if unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of!(limits).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `process` is the live child's process handle.
            if unsafe { AssignProcessToJobObject(job.0, process) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        pub fn terminate(&self) -> io::Result<()> {
            // SAFETY: the job handle is open until drop.
            if unsafe { TerminateJobObject(self.0, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle was created by CreateJobObjectW and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}
