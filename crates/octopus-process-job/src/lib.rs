//! Windows job objects: a job holds a process and every process it starts,
//! and terminating or dropping the job kills them all. It is the Windows
//! counterpart of a Unix process group that is killed as a whole. Elsewhere
//! this crate is empty.

#[cfg(windows)]
pub use windows::{Job, NamedJob};

#[cfg(windows)]
mod windows {
    use std::io;
    use std::os::windows::io::RawHandle;

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, HANDLE,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
        JobObjectExtendedLimitInformation, OpenJobObjectW, QueryInformationJobObject,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::SystemServices::{
        JOB_OBJECT_ASSIGN_PROCESS, JOB_OBJECT_QUERY, JOB_OBJECT_TERMINATE,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

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

    /// A job known by name, so that a later process can find it again: the
    /// counterpart of a Unix process group whose id is recorded on disk. It is
    /// not killed when its handles close; it lasts while any handle or any
    /// process in it remains, so its processes outlive the process that
    /// created it until someone terminates the job.
    pub struct NamedJob(HANDLE);

    // SAFETY: a job handle may be used and closed from any thread.
    unsafe impl Send for NamedJob {}
    // SAFETY: as above; the methods only pass the handle to thread-safe calls.
    unsafe impl Sync for NamedJob {}

    impl NamedJob {
        /// Creates the job, or opens it if it already exists; says which.
        pub fn create_or_open(name: &str) -> io::Result<(Self, bool)> {
            let name = wide(name);
            // SAFETY: `name` is a NUL-terminated wide string; null attributes
            // give the job the caller's default security.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), name.as_ptr()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: read immediately after the call it describes.
            let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            Ok((Self(handle), existed))
        }

        /// Opens an existing job, or `None` if no job has this name.
        pub fn open(name: &str) -> io::Result<Option<Self>> {
            let name = wide(name);
            // SAFETY: `name` is a NUL-terminated wide string.
            let handle = unsafe {
                OpenJobObjectW(JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE | JOB_OBJECT_ASSIGN_PROCESS, 0, name.as_ptr())
            };
            if handle.is_null() {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) { Ok(None) } else { Err(error) };
            }
            Ok(Some(Self(handle)))
        }

        /// Adds the process `process_id` (and so every process it starts
        /// afterwards) to the job.
        pub fn assign(&self, process_id: u32) -> io::Result<()> {
            // SAFETY: plain call; the returned handle is closed below.
            let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, process_id) };
            if process.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: both handles are open for the call.
            let assigned = unsafe { AssignProcessToJobObject(self.0, process) };
            let error = io::Error::last_os_error();
            // SAFETY: `process` came from OpenProcess and is closed once.
            unsafe { CloseHandle(process) };
            if assigned == 0 { Err(error) } else { Ok(()) }
        }

        /// How many processes in the job are still running.
        pub fn active_processes(&self) -> io::Result<u32> {
            // SAFETY: an all-zero accounting structure is valid output space.
            let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: `accounting` is the structure this information class fills.
            if unsafe {
                QueryInformationJobObject(
                    self.0,
                    JobObjectBasicAccountingInformation,
                    std::ptr::addr_of_mut!(accounting).cast(),
                    std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(accounting.ActiveProcesses)
        }

        /// Terminates every process in the job.
        pub fn terminate(&self) -> io::Result<()> {
            // SAFETY: the job handle is open until drop.
            if unsafe { TerminateJobObject(self.0, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for NamedJob {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateJobObjectW or OpenJobObjectW and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }
}
