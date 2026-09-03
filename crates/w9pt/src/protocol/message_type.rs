//! Numeric message assignments for the supported stock dialect.

/// A supported `9P2000.L` wire message number.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum MessageType {
    /// Linux error response.
    Rlerror = 7,
    /// Filesystem statistics request.
    Tstatfs = 8,
    /// Filesystem statistics response.
    Rstatfs = 9,
    /// Linux open request.
    Tlopen = 12,
    /// Linux open response.
    Rlopen = 13,
    /// Linux create request.
    Tlcreate = 14,
    /// Linux create response.
    Rlcreate = 15,
    /// Symbolic-link creation request.
    Tsymlink = 16,
    /// Symbolic-link creation response.
    Rsymlink = 17,
    /// Device-node creation request.
    Tmknod = 18,
    /// Device-node creation response.
    Rmknod = 19,
    /// Fid-based rename request.
    Trename = 20,
    /// Fid-based rename response.
    Rrename = 21,
    /// Symbolic-link read request.
    Treadlink = 22,
    /// Symbolic-link read response.
    Rreadlink = 23,
    /// Attribute query request.
    Tgetattr = 24,
    /// Attribute query response.
    Rgetattr = 25,
    /// Attribute update request.
    Tsetattr = 26,
    /// Attribute update response.
    Rsetattr = 27,
    /// Extended-attribute walk request.
    Txattrwalk = 30,
    /// Extended-attribute walk response.
    Rxattrwalk = 31,
    /// Extended-attribute create request.
    Txattrcreate = 32,
    /// Extended-attribute create response.
    Rxattrcreate = 33,
    /// Directory read request.
    Treaddir = 40,
    /// Directory read response.
    Rreaddir = 41,
    /// Durability barrier request.
    Tfsync = 50,
    /// Durability barrier response.
    Rfsync = 51,
    /// Record-lock request.
    Tlock = 52,
    /// Record-lock response.
    Rlock = 53,
    /// Record-lock query request.
    Tgetlock = 54,
    /// Record-lock query response.
    Rgetlock = 55,
    /// Hard-link request.
    Tlink = 70,
    /// Hard-link response.
    Rlink = 71,
    /// Directory creation request.
    Tmkdir = 72,
    /// Directory creation response.
    Rmkdir = 73,
    /// Directory-relative rename request.
    Trenameat = 74,
    /// Directory-relative rename response.
    Rrenameat = 75,
    /// Directory-relative unlink request.
    Tunlinkat = 76,
    /// Directory-relative unlink response.
    Runlinkat = 77,
    /// Version negotiation request.
    Tversion = 100,
    /// Version negotiation response.
    Rversion = 101,
    /// Authentication exchange request.
    Tauth = 102,
    /// Authentication exchange response.
    Rauth = 103,
    /// Export attach request.
    Tattach = 104,
    /// Export attach response.
    Rattach = 105,
    /// Request flush.
    Tflush = 108,
    /// Request flush response.
    Rflush = 109,
    /// Path walk request.
    Twalk = 110,
    /// Path walk response.
    Rwalk = 111,
    /// Positioned read request.
    Tread = 116,
    /// Positioned read response.
    Rread = 117,
    /// Positioned write request.
    Twrite = 118,
    /// Positioned write response.
    Rwrite = 119,
    /// Fid retirement request.
    Tclunk = 120,
    /// Fid retirement response.
    Rclunk = 121,
    /// Remove-by-fid request.
    Tremove = 122,
    /// Remove-by-fid response.
    Rremove = 123,
}

impl MessageType {
    /// Returns the exact one-byte wire assignment.
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// Reports whether clients are permitted to send this message.
    pub const fn is_request(self) -> bool {
        matches!(
            self,
            Self::Tstatfs
                | Self::Tlopen
                | Self::Tlcreate
                | Self::Tsymlink
                | Self::Tmknod
                | Self::Trename
                | Self::Treadlink
                | Self::Tgetattr
                | Self::Tsetattr
                | Self::Txattrwalk
                | Self::Txattrcreate
                | Self::Treaddir
                | Self::Tfsync
                | Self::Tlock
                | Self::Tgetlock
                | Self::Tlink
                | Self::Tmkdir
                | Self::Trenameat
                | Self::Tunlinkat
                | Self::Tversion
                | Self::Tauth
                | Self::Tattach
                | Self::Tflush
                | Self::Twalk
                | Self::Tread
                | Self::Twrite
                | Self::Tclunk
                | Self::Tremove
        )
    }
}

impl TryFrom<u8> for MessageType {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        let message = match value {
            7 => Self::Rlerror,
            8 => Self::Tstatfs,
            9 => Self::Rstatfs,
            12 => Self::Tlopen,
            13 => Self::Rlopen,
            14 => Self::Tlcreate,
            15 => Self::Rlcreate,
            16 => Self::Tsymlink,
            17 => Self::Rsymlink,
            18 => Self::Tmknod,
            19 => Self::Rmknod,
            20 => Self::Trename,
            21 => Self::Rrename,
            22 => Self::Treadlink,
            23 => Self::Rreadlink,
            24 => Self::Tgetattr,
            25 => Self::Rgetattr,
            26 => Self::Tsetattr,
            27 => Self::Rsetattr,
            30 => Self::Txattrwalk,
            31 => Self::Rxattrwalk,
            32 => Self::Txattrcreate,
            33 => Self::Rxattrcreate,
            40 => Self::Treaddir,
            41 => Self::Rreaddir,
            50 => Self::Tfsync,
            51 => Self::Rfsync,
            52 => Self::Tlock,
            53 => Self::Rlock,
            54 => Self::Tgetlock,
            55 => Self::Rgetlock,
            70 => Self::Tlink,
            71 => Self::Rlink,
            72 => Self::Tmkdir,
            73 => Self::Rmkdir,
            74 => Self::Trenameat,
            75 => Self::Rrenameat,
            76 => Self::Tunlinkat,
            77 => Self::Runlinkat,
            100 => Self::Tversion,
            101 => Self::Rversion,
            102 => Self::Tauth,
            103 => Self::Rauth,
            104 => Self::Tattach,
            105 => Self::Rattach,
            108 => Self::Tflush,
            109 => Self::Rflush,
            110 => Self::Twalk,
            111 => Self::Rwalk,
            116 => Self::Tread,
            117 => Self::Rread,
            118 => Self::Twrite,
            119 => Self::Rwrite,
            120 => Self::Tclunk,
            121 => Self::Rclunk,
            122 => Self::Tremove,
            123 => Self::Rremove,
            _ => return Err(value),
        };
        Ok(message)
    }
}

/// Boundary responsible for advancing a valid request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchClass {
    /// Completed entirely from connection protocol state.
    Protocol,
    /// Requires a host policy/authentication decision.
    Policy,
    /// Normally filesystem work but may be policy work for an authentication fid.
    FilesystemOrPolicy,
    /// Requires backend-neutral filesystem work.
    Filesystem,
}

/// One intentional decode/encode/dispatch entry in the supported operation matrix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationSpec {
    /// Client request type accepted by the decoder.
    pub request: MessageType,
    /// Normal success response emitted by the encoder.
    pub response: MessageType,
    /// State-machine boundary used to execute the request.
    pub dispatch: DispatchClass,
}

/// Exhaustive stock `9P2000.L` subset implemented by the crate.
pub const OPERATION_MATRIX: [OperationSpec; 28] = [
    OperationSpec {
        request: MessageType::Tversion,
        response: MessageType::Rversion,
        dispatch: DispatchClass::Protocol,
    },
    OperationSpec {
        request: MessageType::Tauth,
        response: MessageType::Rauth,
        dispatch: DispatchClass::Policy,
    },
    OperationSpec {
        request: MessageType::Tattach,
        response: MessageType::Rattach,
        dispatch: DispatchClass::Policy,
    },
    OperationSpec {
        request: MessageType::Tflush,
        response: MessageType::Rflush,
        dispatch: DispatchClass::Protocol,
    },
    OperationSpec {
        request: MessageType::Twalk,
        response: MessageType::Rwalk,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tclunk,
        response: MessageType::Rclunk,
        dispatch: DispatchClass::FilesystemOrPolicy,
    },
    OperationSpec {
        request: MessageType::Tlopen,
        response: MessageType::Rlopen,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tlcreate,
        response: MessageType::Rlcreate,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tmkdir,
        response: MessageType::Rmkdir,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tmknod,
        response: MessageType::Rmknod,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tsymlink,
        response: MessageType::Rsymlink,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tread,
        response: MessageType::Rread,
        dispatch: DispatchClass::FilesystemOrPolicy,
    },
    OperationSpec {
        request: MessageType::Twrite,
        response: MessageType::Rwrite,
        dispatch: DispatchClass::FilesystemOrPolicy,
    },
    OperationSpec {
        request: MessageType::Treaddir,
        response: MessageType::Rreaddir,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tfsync,
        response: MessageType::Rfsync,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tstatfs,
        response: MessageType::Rstatfs,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tgetattr,
        response: MessageType::Rgetattr,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tsetattr,
        response: MessageType::Rsetattr,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Treadlink,
        response: MessageType::Rreadlink,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Trename,
        response: MessageType::Rrename,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Trenameat,
        response: MessageType::Rrenameat,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tremove,
        response: MessageType::Rremove,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tunlinkat,
        response: MessageType::Runlinkat,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tlink,
        response: MessageType::Rlink,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Txattrwalk,
        response: MessageType::Rxattrwalk,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Txattrcreate,
        response: MessageType::Rxattrcreate,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tlock,
        response: MessageType::Rlock,
        dispatch: DispatchClass::Filesystem,
    },
    OperationSpec {
        request: MessageType::Tgetlock,
        response: MessageType::Rgetlock,
        dispatch: DispatchClass::Filesystem,
    },
];
