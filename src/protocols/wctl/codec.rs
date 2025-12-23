use std::io::Read;
use std::io::Write;

use rkyv::api::high::HighDeserializer;
use rkyv::api::high::HighValidator;
use rkyv::bytecheck;
use rkyv::rancor::Error as RancorError;
use rkyv::util::AlignedVec;

use crate::prelude::*;
use crate::protocols::wprs::serializer::Serializable;
use crate::protocols::wprs::framing::Framed;

pub fn send<T>(stream: &mut impl Write, msg: &T) -> Result<()>
where
    T: Serializable,
{
    let data = rkyv::to_bytes::<RancorError>(msg).location(loc!())?;
    data.framed_write(stream).location(loc!())?;
    stream.flush().location(loc!())?;
    Ok(())
}

pub fn recv<T>(stream: &mut impl Read) -> Result<T>
where
    T: Serializable,
    T::Archived: rkyv::Deserialize<T, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    let data = AlignedVec::framed_read(stream).location(loc!())?;
    rkyv::from_bytes::<T, RancorError>(&data).location(loc!())
}
