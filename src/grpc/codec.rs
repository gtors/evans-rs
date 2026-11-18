use prost_reflect::prost::Message;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use tonic::Status;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};

#[derive(Debug, Clone)]
pub struct DynamicCodec {
    desc: MessageDescriptor,
}

impl DynamicCodec {
    pub fn new(desc: MessageDescriptor) -> Self {
        Self { desc }
    }
}

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;

    type Encoder = DynamicEncoder;
    type Decoder = DynamicDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynamicEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        DynamicDecoder {
            desc: self.desc.clone(),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct DynamicEncoder;

impl Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        item.encode(dst)
            .map_err(|e| Status::internal(format!("failed to encode request message: {e}")))
    }
}

#[derive(Debug, Clone)]
pub struct DynamicDecoder {
    desc: MessageDescriptor,
}

impl Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        DynamicMessage::decode(self.desc.clone(), src)
            .map(Some)
            .map_err(|e| Status::internal(format!("failed to decode response message: {e}")))
    }
}
