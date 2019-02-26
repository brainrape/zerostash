use crate::backends::*;
use crate::compress::{self, STREAM_BLOCK_SIZE};
use crate::crypto::CryptoProvider;
use crate::meta::{
    Encoder, Field, FieldOffset, FieldWriter, MetaObjectField, MetaObjectHeader, ObjectIndex,
    HEADER_SIZE,
};
use crate::objects::{ObjectId, WriteObject};

use failure::Error;
use serde::Serialize;
use serde_cbor::{ser::to_vec as serialize_to_vec, ser::to_writer as serialize_to_writer};

use std::collections::HashMap;
use std::io::{self, Seek, SeekFrom, Write};

pub struct Writer<B, C> {
    objects: ObjectIndex,
    offsets: Vec<FieldOffset>,
    encoder: WriteState,
    current_field: Option<Field>,
    backend: B,
    crypto: C,
}

impl<B, C> FieldWriter for Writer<B, C>
where
    B: Backend,
    C: CryptoProvider,
{
    fn write_next(&mut self, obj: impl Serialize) {
        let writer = self.encoder.writer().unwrap();
        let capacity = writer.capacity();
        let position = writer.position();

        if capacity - position < STREAM_BLOCK_SIZE {
            self.seal_and_store();
        }

        serialize_to_writer(self.encoder.start().unwrap(), &obj).unwrap();
    }
}

impl<B, C> Writer<B, C>
where
    B: Backend,
    C: CryptoProvider,
{
    pub fn new(root_object_id: ObjectId, backend: B, crypto: C) -> Result<Writer<B, C>, Error> {
        let mut object = WriteObject::default().reserve_tag(crypto.tag_len());
        object.set_id(root_object_id);
        object.seek(SeekFrom::Start(HEADER_SIZE as u64)).unwrap();

        Ok(Writer {
            encoder: WriteState::Parked(object),
            offsets: vec![],
            objects: HashMap::new(),
            current_field: None,
            backend,
            crypto,
        })
    }

    pub fn objects(&self) -> &ObjectIndex {
        &self.objects
    }

    pub fn write_field(&mut self, f: Field, obj: &impl MetaObjectField) {
        // book keeping
        self.offsets
            .push(f.as_offset(self.encoder.writer().unwrap().position() as u32));
        self.objects
            .entry(f.clone())
            .or_default()
            .insert(self.encoder.writer().unwrap().id);

        self.encoder.start().unwrap();

        // clean up
        self.current_field = Some(f);
        obj.serialize(self);
        self.current_field = None;

        // skip to next multiple of STREAM_BLOCK_SIZE
        let mut object = self.encoder.finish().unwrap();
        let skip = STREAM_BLOCK_SIZE - (object.position() - HEADER_SIZE) % STREAM_BLOCK_SIZE;
        object.seek(SeekFrom::Current(skip as i64)).unwrap();
        self.encoder = WriteState::Parked(object);
    }

    pub fn seal_and_store(&mut self) {
        let mut object = self.encoder.finish().unwrap();
        let end = object.position();

        // fill the end of the object with random & other stuff
        object.finalize(&self.crypto);
        let next_object_id = ObjectId::new(&self.crypto);

        let object_header = MetaObjectHeader::new(
            self.current_field.clone().map(|_| next_object_id),
            &self.offsets,
            end,
        );
        let header_bytes = serialize_to_vec(&object_header).expect("failed to write header");

        // ok, this is pretty rough, but it also shouldn't happen, so yolo
        assert!(header_bytes.len() < HEADER_SIZE);
        object.write_head(&header_bytes);

        // encrypt & store
        self.crypto.encrypt_object(&mut object);
        self.backend.write_object(&object).unwrap();

        // track which objects are holding what kind of data
        for fo in self.offsets.drain(..) {
            self.objects
                .entry(fo.as_field())
                .or_default()
                .insert(object.id);
        }

        // start cleaning up and bookkeeping
        object.set_id(next_object_id);

        // re-initialize the object
        object.clear();
        object.seek(SeekFrom::Start(HEADER_SIZE as u64)).unwrap();
        self.encoder = WriteState::Parked(object);

        // make sure we register the currently written field in the new object
        if let Some(f) = &self.current_field {
            self.offsets.push(f.as_offset(HEADER_SIZE as u32));
        }
    }
}

enum WriteState {
    Idle,
    Parked(WriteObject),
    Encoding(Encoder),
}

impl WriteState {
    fn start(&mut self) -> Result<&mut Self, Error> {
        use WriteState::*;

        match self {
            Idle => Err(format_err!("Uninitialized")),
            Parked(_) => {
                let mut tmp = WriteState::Idle;
                std::mem::swap(&mut tmp, self);

                let encoder = match tmp {
                    Parked(w) => compress::stream(w)?,
                    _ => unreachable!(),
                };

                std::mem::replace(self, WriteState::Encoding(encoder));
                Ok(self)
            }
            Encoding(_) => Ok(self),
        }
    }

    fn finish(&mut self) -> Result<WriteObject, Error> {
        use WriteState::*;

        let mut encoder = WriteState::Idle;
        std::mem::swap(self, &mut encoder);

        match encoder {
            Idle => Err(format_err!("Uninitialized")),
            Parked(w) => Ok(w),
            Encoding(e) => {
                let (object, err) = e.finish();
                err?;

                Ok(object)
            }
        }
    }

    fn writer(&self) -> Result<&WriteObject, Error> {
        use WriteState::*;
        match self {
            Idle => Err(format_err!("Uninitialized")),
            Parked(w) => Ok(w),
            Encoding(e) => Ok(e.writer()),
        }
    }
}

impl Write for WriteState {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        use io::{Error, ErrorKind};
        use WriteState::*;

        match self {
            Idle => Err(Error::new(ErrorKind::Other, "Uninitialized")),
            Parked(_) => Err(Error::new(ErrorKind::Other, "Inactive")),
            Encoding(e) => e.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}