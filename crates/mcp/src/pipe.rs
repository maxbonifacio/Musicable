//! The local pipe agents reach Musicable through, which only the user's
//! account can open: a socket in the user's own folder on macOS and Linux,
//! and a named pipe on Windows. Nothing listens on the network.
//!
//! On macOS and Linux the socket is `agents/socket` in the config folder,
//! in a folder only the user can open, and each end checks that the other
//! runs as the same user. On Windows the pipe's name holds the user's SID
//! and a hash of the config folder's path, so each user and each config
//! folder has a pipe of its own. Only the user's account can open it, other
//! computers are turned away, and the bridge checks that the user's account
//! made it, so another account can't stand in for Musicable by making it
//! first.

use std::fmt;
use std::io;
use std::path::Path;

/// Where Musicable listens for agents, for the config folder it uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address(sys::Address);

impl Address {
    /// The address for the config folder `config_dir`.
    pub fn new(config_dir: &Path) -> io::Result<Self> {
        sys::address(config_dir).map(Self)
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Listens at an address, until it's dropped.
pub(crate) struct Listener(sys::Listener);

/// A connection through the pipe, to or from Musicable.
pub(crate) type Stream = sys::Stream;

/// Listens at `address`. Fails if another Musicable listens there already.
pub(crate) async fn listen(address: &Address) -> io::Result<Listener> {
    sys::listen(&address.0).await.map(Listener)
}

impl Listener {
    /// The next connection from the user's own account.
    pub(crate) async fn accept(&mut self) -> io::Result<sys::Accepted> {
        self.0.accept().await
    }
}

/// Connects to the Musicable listening at `address`, which must be the
/// user's own.
pub(crate) async fn connect(address: &Address) -> io::Result<Stream> {
    sys::connect(&address.0).await
}

/// That another Musicable lets agents in already, at `address`.
fn taken(address: impl fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::AddrInUse,
        format!("another Musicable lets agents in already, at {address}"),
    )
}

#[cfg(unix)]
mod sys {
    use std::fmt;
    use std::fs;
    use std::io;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};

    use tokio::net::{UnixListener, UnixStream};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Address {
        /// The folder only the user can open, which the socket is in.
        folder: PathBuf,
        socket: PathBuf,
    }

    impl fmt::Display for Address {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            self.socket.display().fmt(f)
        }
    }

    pub type Stream = UnixStream;
    pub type Accepted = UnixStream;

    pub fn address(config_dir: &Path) -> io::Result<Address> {
        let folder = config_dir.join("agents");
        Ok(Address {
            socket: folder.join("socket"),
            folder,
        })
    }

    pub struct Listener {
        listener: UnixListener,
        socket: PathBuf,
    }

    pub async fn listen(address: &Address) -> io::Result<Listener> {
        fs::create_dir_all(&address.folder)?;
        let folder = fs::metadata(&address.folder)?;
        if folder.uid() != user() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} belongs to another user", address.folder.display()),
            ));
        }
        fs::set_permissions(&address.folder, fs::Permissions::from_mode(0o700))?;
        // A socket left by a Musicable that stopped without taking it away
        // goes, unless a Musicable still listens there.
        if fs::symlink_metadata(&address.socket).is_ok() {
            if UnixStream::connect(&address.socket).await.is_ok() {
                return Err(super::taken(address));
            }
            fs::remove_file(&address.socket)?;
        }
        let listener = UnixListener::bind(&address.socket)?;
        fs::set_permissions(&address.socket, fs::Permissions::from_mode(0o600))?;
        Ok(Listener {
            listener,
            socket: address.socket.clone(),
        })
    }

    impl Listener {
        pub async fn accept(&mut self) -> io::Result<UnixStream> {
            loop {
                let (stream, _) = self.listener.accept().await?;
                if same_user(&stream) {
                    return Ok(stream);
                }
            }
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.socket);
        }
    }

    pub async fn connect(address: &Address) -> io::Result<UnixStream> {
        let stream = UnixStream::connect(&address.socket).await?;
        if !same_user(&stream) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("another user listens at {address}"),
            ));
        }
        Ok(stream)
    }

    /// Whether the other end of `stream` runs as the user.
    fn same_user(stream: &UnixStream) -> bool {
        stream.peer_cred().is_ok_and(|cred| cred.uid() == user())
    }

    /// The user's id.
    fn user() -> u32 {
        // SAFETY: geteuid can't fail, and touches no memory of ours.
        unsafe { libc::geteuid() }
    }
}

#[cfg(windows)]
mod sys {
    use std::ffi::c_void;
    use std::fmt;
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, ERROR_SUCCESS, HANDLE, HLOCAL, LocalFree,
    };
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
    };
    use windows::Win32::Security::{
        GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::{HSTRING, PWSTR};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Address {
        name: String,
        /// The user's SID, as text: "S-1-5-21-...".
        user: String,
    }

    impl fmt::Display for Address {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.name)
        }
    }

    pub type Stream = NamedPipeClient;
    pub type Accepted = NamedPipeServer;

    pub fn address(config_dir: &Path) -> io::Result<Address> {
        let user = user()?;
        // Windows' paths don't care about case.
        let folder = config_dir.to_string_lossy().to_lowercase();
        let name = format!(
            r"\\.\pipe\musicable-agents-{user}-{:016x}",
            fnv1a(folder.as_bytes())
        );
        Ok(Address { name, user })
    }

    /// The 64-bit FNV-1a hash of `bytes`, which doesn't change from one
    /// build to the next, as the bridge and the app must agree on it.
    fn fnv1a(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    pub struct Listener {
        /// The pipe's next instance, which the next agent connects to.
        next: NamedPipeServer,
        address: Address,
    }

    pub async fn listen(address: &Address) -> io::Result<Listener> {
        let next = create(address, true).map_err(|error| {
            match error.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32) {
                true => super::taken(address),
                false => error,
            }
        })?;
        Ok(Listener {
            next,
            address: address.clone(),
        })
    }

    impl Listener {
        pub async fn accept(&mut self) -> io::Result<NamedPipeServer> {
            self.next.connect().await?;
            let next = create(&self.address, false)?;
            Ok(std::mem::replace(&mut self.next, next))
        }
    }

    /// An instance of the pipe at `address` that only the user can open,
    /// and only from this computer: the pipe's first, if `first`, which
    /// fails if the pipe is there already.
    fn create(address: &Address, first: bool) -> io::Result<NamedPipeServer> {
        // The user as its owner, which the bridge checks: an administrator's
        // elevated Musicable would make the Administrators group its owner
        // otherwise. Full access for the user, and none for anyone else.
        let sddl = HSTRING::from(format!("O:{0}D:P(A;;GA;;;{0})", address.user));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: the descriptor is LocalFree'd below.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &sddl,
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }?;
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let mut options = ServerOptions::new();
        options
            .first_pipe_instance(first)
            .reject_remote_clients(true);
        // SAFETY: the attributes, and the descriptor they point to, live
        // through the call.
        let server = unsafe {
            options.create_with_security_attributes_raw(
                &address.name,
                (&raw mut attributes).cast::<c_void>(),
            )
        };
        // SAFETY: the descriptor came from LocalAlloc, and isn't used again.
        unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        server
    }

    pub async fn connect(address: &Address) -> io::Result<NamedPipeClient> {
        let client = loop {
            match ClientOptions::new().open(&address.name) {
                Ok(client) => break client,
                // Each instance is taken: one is on its way.
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => return Err(error),
            }
        };
        let handle = HANDLE(client.as_raw_handle());
        if owner(handle)? != address.user {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("another user made {address}"),
            ));
        }
        Ok(client)
    }

    /// The SID of the account that made the kernel object `handle` is a
    /// handle to, as text.
    fn owner(handle: HANDLE) -> io::Result<String> {
        let mut owner = PSID::default();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: the owner points into the descriptor, which is LocalFree'd
        // once the owner is read.
        let error = unsafe {
            GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION,
                Some(&mut owner),
                None,
                None,
                None,
                Some(&mut descriptor),
            )
        };
        if error != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(error.0 as i32));
        }
        let text = sid_text(owner);
        // SAFETY: as above.
        unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        text
    }

    /// The SID of the user Musicable runs as, as text.
    fn user() -> io::Result<String> {
        let mut token = HANDLE::default();
        // SAFETY: the token is closed below.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }?;
        let user = token_user(token);
        // SAFETY: the token was opened above, and isn't used again.
        let _ = unsafe { CloseHandle(token) };
        user
    }

    fn token_user(token: HANDLE) -> io::Result<String> {
        let mut size = 0;
        // SAFETY: with no buffer, it only says how big one must be, and
        // fails for that.
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut size) };
        // In u64s, so the TOKEN_USER in it lines up.
        let mut buffer = vec![0_u64; (size as usize).div_ceil(8)];
        // SAFETY: the buffer holds `size` bytes.
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                size,
                &mut size,
            )
        }?;
        // SAFETY: GetTokenInformation put a TOKEN_USER at the buffer's
        // start, whose SID points into the buffer, which outlives it.
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        sid_text(user.User.Sid)
    }

    /// `sid` as text: "S-1-5-21-...".
    fn sid_text(sid: PSID) -> io::Result<String> {
        let mut text = PWSTR::null();
        // SAFETY: `sid` is valid, and the text is LocalFree'd below.
        unsafe { ConvertSidToStringSidW(sid, &mut text) }?;
        // SAFETY: the text ends in a nul, as ConvertSidToStringSidW made it.
        let sid = unsafe { text.to_string() }.map_err(io::Error::other);
        // SAFETY: the text came from LocalAlloc, and isn't used again.
        unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
        sid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_config_folder_has_an_address_of_its_own() {
        let a = Address::new(Path::new("/home/a/.config/musicable")).unwrap();
        let b = Address::new(Path::new("/home/b/.config/musicable")).unwrap();
        assert_ne!(a, b);
        assert_eq!(
            a,
            Address::new(Path::new("/home/a/.config/musicable")).unwrap()
        );
        if cfg!(windows) {
            assert!(a.to_string().starts_with(r"\\.\pipe\musicable-agents-S-1-"));
        } else {
            assert_eq!(a.to_string(), "/home/a/.config/musicable/agents/socket");
        }
    }
}
