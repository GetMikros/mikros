//! The client and server of each protocol.
//!
//! For protocol `P` with method `M`, the bindings define the marker type
//! `P`, `PClient` and `PServer`, the argument structure `PMRequest`, for
//! a two-way method the result structure `PMResponse` and the
//! `PMResponder` that replies, for an event the structure `PMEvent`, and
//! the enumerations `PRequest` of the requests a server receives and
//! `PEvent` of the events a client receives. `PAsyncClient` and
//! `PAsyncServer` do the same on the executor.

use super::{Gen, camel, doc, snake};
use crate::model::*;
use std::fmt::Write;

impl Gen<'_> {
    pub(super) fn protocol(&self, name: &str, methods: &[Method], out: &mut String) {
        let rt = self.rt();
        writeln!(out, "#[derive(Debug, PartialEq)]\npub struct {name};\n").unwrap();
        let (mut client, mut server, mut requests) = (String::new(), String::new(), String::new());
        let mut async_client = String::new();
        let (mut request_arms, mut event_arms, mut event_variants) =
            (String::new(), String::new(), String::new());
        for m in methods {
            let variant = camel(&m.name);
            let func = snake(&m.name);
            let ordinal = m.ordinal;
            let flags = if m.strictness == Strictness::Flexible {
                format!("{rt}::FLAG_FLEXIBLE")
            } else {
                "0".into()
            };
            let flexible = m.strictness == Strictness::Flexible;
            match &m.kind {
                MethodKind::TwoWay {
                    request,
                    response,
                    error,
                } => {
                    let req = format!("{name}{variant}Request");
                    let resp = format!("{name}{variant}Response");
                    let responder = format!("{name}{variant}Responder");
                    self.structure(&req, request, out);
                    self.structure(&resp, response, out);
                    let (params, build) = self.params(&req, request);
                    let result = match error {
                        Some(e) => format!("Result<{resp}, {}>", self.rust_type(e)),
                        None => resp.clone(),
                    };
                    client.push_str(&doc(&m.doc, ""));
                    writeln!(
                        client,
                        "pub fn {func}(&self, {params}deadline: i64) -> Result<{result}, {rt}::Error> {{\nlet mut __message = {rt}::Encoder::new({rt}::Header {{ txid: 0, ordinal: {ordinal}, flags: {flags} }}, {req}::__SIZE);\n{build}.__encode(&mut __message, {rt}::HEADER_SIZE)?;\nlet (bytes, handles) = self.channel.call(__message, deadline)?;\nself.channel.checked(Self::__{func}_reply(&bytes, handles))\n}}\n\nfn __{func}_reply(bytes: &[u8], handles: ::alloc::vec::Vec<{rt}::RawHandle>) -> Result<{result}, {rt}::Error> {{\nlet mut decoder = {rt}::Decoder::new(bytes, handles);\nlet d = &mut decoder;\nlet header = d.header()?;\nif header.ordinal != {ordinal} || header.flags & {rt}::FLAG_FLEXIBLE != 0 {{\nreturn Err({rt}::Error::Invalid);\n}}\nif header.flags & {rt}::FLAG_UNKNOWN_METHOD != 0 {{\ndecoder.finish()?;\nreturn Err({rt}::Error::UnknownMethod);\n}}\n{}decoder.finish()?;\nOk(result)\n}}\n",
                        self.decode_result(&resp, error.as_ref())
                    )
                    .unwrap();
                    async_client.push_str(&doc(&m.doc, ""));
                    writeln!(
                        async_client,
                        "pub async fn {func}(&self, {params}) -> Result<{result}, {rt}::Error> {{\nlet mut __message = {rt}::Encoder::new({rt}::Header {{ txid: 0, ordinal: {ordinal}, flags: {flags} }}, {req}::__SIZE);\n{build}.__encode(&mut __message, {rt}::HEADER_SIZE)?;\nlet (bytes, handles) = self.channel.call(__message).await?;\nself.channel.checked({name}Client::__{func}_reply(&bytes, handles))\n}}\n"
                    )
                    .unwrap();
                    writeln!(
                        requests,
                        "{}    {variant}({req}, {responder}),",
                        doc(&m.doc, "    ")
                    )
                    .unwrap();
                    writeln!(
                        request_arms,
                        "{ordinal} => {{\nif header.txid == 0 || (header.flags & {rt}::FLAG_FLEXIBLE != 0) != {flexible} {{\nreturn Err({rt}::Error::Invalid);\n}}\nlet at = d.claim({req}::__SIZE)?;\nlet request = {req}::__decode(d, at)?;\ndecoder.finish()?;\nOk(Some({name}Request::{variant}(request, {responder} {{ txid: header.txid }})))\n}}"
                    )
                    .unwrap();
                    let (body, encode) = self.encode_result(&resp, error.as_ref());
                    writeln!(
                        out,
                        "/// Replies to one `{}` request.\n#[derive(Debug, PartialEq)]\npub struct {responder} {{\ntxid: u32,\n}}\n\nimpl {responder} {{\npub fn reply(self, server: &{name}Server, result: {result}) -> Result<(), {rt}::Error> {{\nlet mut __message = {rt}::Encoder::new({rt}::Header {{ txid: self.txid, ordinal: {ordinal}, flags: 0 }}, {body});\nlet e = &mut __message;\nlet at = {rt}::HEADER_SIZE;\n{encode}server.channel.send(__message)\n}}\n}}\n",
                        m.name
                    )
                    .unwrap();
                }
                MethodKind::OneWay(args) => {
                    let req = format!("{name}{variant}Request");
                    self.structure(&req, args, out);
                    let (params, build) = self.params(&req, args);
                    client.push_str(&doc(&m.doc, ""));
                    writeln!(
                        client,
                        "pub fn {func}(&self, {params}) -> Result<(), {rt}::Error> {{\nlet mut __message = {rt}::Encoder::new({rt}::Header {{ txid: 0, ordinal: {ordinal}, flags: {flags} }}, {req}::__SIZE);\n{build}.__encode(&mut __message, {rt}::HEADER_SIZE)?;\nself.channel.send(__message)\n}}\n"
                    )
                    .unwrap();
                    async_client.push_str(&doc(&m.doc, ""));
                    writeln!(
                        async_client,
                        "pub fn {func}(&self, {params}) -> Result<(), {rt}::Error> {{\nlet mut __message = {rt}::Encoder::new({rt}::Header {{ txid: 0, ordinal: {ordinal}, flags: {flags} }}, {req}::__SIZE);\n{build}.__encode(&mut __message, {rt}::HEADER_SIZE)?;\nself.channel.send(__message)\n}}\n"
                    )
                    .unwrap();
                    writeln!(requests, "{}    {variant}({req}),", doc(&m.doc, "    ")).unwrap();
                    writeln!(
                        request_arms,
                        "{ordinal} => {{\nif header.txid != 0 || (header.flags & {rt}::FLAG_FLEXIBLE != 0) != {flexible} {{\nreturn Err({rt}::Error::Invalid);\n}}\nlet at = d.claim({req}::__SIZE)?;\nlet request = {req}::__decode(d, at)?;\ndecoder.finish()?;\nOk(Some({name}Request::{variant}(request)))\n}}"
                    )
                    .unwrap();
                }
                MethodKind::Event(args) => {
                    let ev = format!("{name}{variant}Event");
                    self.structure(&ev, args, out);
                    let (params, build) = self.params(&ev, args);
                    server.push_str(&doc(&m.doc, ""));
                    writeln!(
                        server,
                        "pub fn send_{func}(&self, {params}) -> Result<(), {rt}::Error> {{\nlet mut __message = {rt}::Encoder::new({rt}::Header {{ txid: 0, ordinal: {ordinal}, flags: {flags} }}, {ev}::__SIZE);\n{build}.__encode(&mut __message, {rt}::HEADER_SIZE)?;\nself.channel.send(__message)\n}}\n"
                    )
                    .unwrap();
                    writeln!(
                        event_variants,
                        "{}    {variant}({ev}),",
                        doc(&m.doc, "    ")
                    )
                    .unwrap();
                    writeln!(
                        event_arms,
                        "{ordinal} => {{\nif (header.flags & {rt}::FLAG_FLEXIBLE != 0) != {flexible} {{\nreturn Err({rt}::Error::Invalid);\n}}\nlet at = d.claim({ev}::__SIZE)?;\nlet event = {ev}::__decode(d, at)?;\ndecoder.finish()?;\nOk(Some({name}Event::{variant}(event)))\n}}"
                    )
                    .unwrap();
                }
            }
        }
        let has_events = !event_variants.is_empty();
        if has_events {
            writeln!(
                client,
                "/// The next event, waiting for one until `deadline`. Events this\n/// version does not declare are discarded when they are flexible.\npub fn next_event(&self, deadline: i64) -> Result<{name}Event, {rt}::Error> {{\nloop {{\nlet (bytes, handles) = self.channel.receive(deadline)?;\nif let Some(event) = self.channel.checked(Self::__event(&bytes, handles))? {{\nreturn Ok(event);\n}}\n}}\n}}\n\nfn __event(bytes: &[u8], handles: ::alloc::vec::Vec<{rt}::RawHandle>) -> Result<Option<{name}Event>, {rt}::Error> {{\nlet mut decoder = {rt}::Decoder::new(bytes, handles);\nlet d = &mut decoder;\nlet header = d.header()?;\nif header.txid != 0 || header.flags & {rt}::FLAG_UNKNOWN_METHOD != 0 {{\nreturn Err({rt}::Error::Invalid);\n}}\nmatch header.ordinal {{\n{event_arms}_ if header.flags & {rt}::FLAG_FLEXIBLE != 0 => Ok(None),\n_ => Err({rt}::Error::Invalid),\n}}\n}}"
            )
            .unwrap();
            writeln!(
                async_client,
                "/// The next event. Events this version does not declare are\n/// discarded when they are flexible.\npub async fn next_event(&self) -> Result<{name}Event, {rt}::Error> {{\nloop {{\nlet (bytes, handles) = self.channel.next_event().await?;\nif let Some(event) = self.channel.checked({name}Client::__event(&bytes, handles))? {{\nreturn Ok(event);\n}}\n}}\n}}\n"
            )
            .unwrap();
            writeln!(out, "/// The events a `{name}` client receives.\n#[derive(Debug, PartialEq)]\npub enum {name}Event {{\n{event_variants}}}\n").unwrap();
        }
        writeln!(
            out,
            "/// The requests a `{name}` server receives.\n#[derive(Debug, PartialEq)]\npub enum {name}Request {{\n{requests}}}\n\n/// The client side of a `{name}` channel.\npub struct {name}Client {{\nchannel: {rt}::Channel,\n}}\n\nimpl {name}Client {{\npub fn new(end: {rt}::ClientEnd<{name}>) -> {name}Client {{\n{name}Client {{ channel: {rt}::Channel::new(end.into_handle()) }}\n}}\n\n/// The channel, unless a protocol error closed it.\npub fn channel(&self) -> Result<{rt}::RawHandle, {rt}::Error> {{\nself.channel.raw()\n}}\n\n{client}}}\n\n/// The server side of a `{name}` channel.\npub struct {name}Server {{\nchannel: {rt}::Channel,\n}}\n\nimpl {name}Server {{\npub fn new(end: {rt}::ServerEnd<{name}>) -> {name}Server {{\n{name}Server {{ channel: {rt}::Channel::new(end.into_handle()) }}\n}}\n\n/// The channel, unless a protocol error closed it.\npub fn channel(&self) -> Result<{rt}::RawHandle, {rt}::Error> {{\nself.channel.raw()\n}}\n\n/// The next request, waiting for one until `deadline`.\npub fn next(&self, deadline: i64) -> Result<{name}Request, {rt}::Error> {{\nloop {{\nlet (bytes, handles) = self.channel.receive(deadline)?;\nif let Some(request) = self.channel.checked(self.__request(&bytes, handles))? {{\nreturn Ok(request);\n}}\n}}\n}}\n\n/// The next request, if one is queued.\npub fn try_next(&self) -> Result<Option<{name}Request>, {rt}::Error> {{\nwhile let Some((bytes, handles)) = self.channel.try_receive()? {{\nif let Some(request) = self.channel.checked(self.__request(&bytes, handles))? {{\nreturn Ok(Some(request));\n}}\n}}\nOk(None)\n}}\n\n/// A request decoded, or `None` for one this version does not declare,\n/// which is discarded or answered as unknown when it is flexible.\nfn __request(&self, bytes: &[u8], handles: ::alloc::vec::Vec<{rt}::RawHandle>) -> Result<Option<{name}Request>, {rt}::Error> {{\nlet mut decoder = {rt}::Decoder::new(bytes, handles);\nlet d = &mut decoder;\nlet header = d.header()?;\nif header.flags & {rt}::FLAG_UNKNOWN_METHOD != 0 {{\nreturn Err({rt}::Error::Invalid);\n}}\nmatch header.ordinal {{\n{request_arms}_ if header.flags & {rt}::FLAG_FLEXIBLE != 0 && !{name}::__declares(header.ordinal) => {{\ndrop(decoder);\nif header.txid != 0 {{\nlet reply = {rt}::Encoder::new({rt}::Header {{ txid: header.txid, ordinal: header.ordinal, flags: {rt}::FLAG_UNKNOWN_METHOD }}, 0);\nself.channel.send(reply)?;\n}}\nOk(None)\n}}\n_ => Err({rt}::Error::Invalid),\n}}\n}}\n\n{server}}}\n\nimpl {name} {{\n#[doc(hidden)]\npub fn __declares(ordinal: u32) -> bool {{\nmatches!(ordinal, {})\n}}\n}}\n",
            if methods.is_empty() { "0".to_string() } else { methods.iter().map(|m| m.ordinal.to_string()).collect::<Vec<_>>().join(" | ") }
        )
        .unwrap();
        writeln!(
            out,
            "/// The client side of a `{name}` channel on the executor, on which\n/// many calls may be outstanding at once.\npub struct {name}AsyncClient {{\nchannel: {rt}::AsyncChannel,\n}}\n\nimpl {name}AsyncClient {{\npub fn new(end: {rt}::ClientEnd<{name}>, reactor: {rt}::kernel::Reactor) -> {name}AsyncClient {{\n{name}AsyncClient {{ channel: {rt}::AsyncChannel::new(end.into_handle(), reactor) }}\n}}\n\n/// The channel, unless a protocol error closed it.\npub fn channel(&self) -> Result<{rt}::RawHandle, {rt}::Error> {{\nself.channel.raw()\n}}\n\n{async_client}}}\n\n/// The server side of a `{name}` channel on the executor. Replies and\n/// events go through `server`.\npub struct {name}AsyncServer {{\nserver: {name}Server,\nreactor: {rt}::kernel::Reactor,\n}}\n\nimpl {name}AsyncServer {{\npub fn new(end: {rt}::ServerEnd<{name}>, reactor: {rt}::kernel::Reactor) -> {name}AsyncServer {{\n{name}AsyncServer {{ server: {name}Server::new(end), reactor }}\n}}\n\npub fn server(&self) -> &{name}Server {{\n&self.server\n}}\n\n/// The next request.\npub async fn next(&self) -> Result<{name}Request, {rt}::Error> {{\nloop {{\nif let Some(request) = self.server.try_next()? {{\nreturn Ok(request);\n}}\n{rt}::AsyncChannel::wait_readable(&self.reactor, self.server.channel()?).await?;\n}}\n}}\n}}\n"
        )
        .unwrap();
    }

    /// The parameters of a method taking `fields`, each followed by a
    /// comma, and the expression that builds its structure `name`.
    ///
    /// A parameter that would take the name of the `deadline` parameter,
    /// or a name starting with `__` like the method's own variables, gets
    /// a trailing `_`.
    fn params(&self, name: &str, fields: &[Field]) -> (String, String) {
        let param = |f: &Field| {
            let field = super::ident(&f.name);
            if f.name == "deadline" || f.name.starts_with("__") {
                format!("{field}_")
            } else {
                field
            }
        };
        let params: String = fields
            .iter()
            .map(|f| format!("{}: {}, ", param(f), self.rust_type(&f.ty)))
            .collect();
        let names: Vec<String> = fields
            .iter()
            .map(|f| format!("{}: {}", super::ident(&f.name), param(f)))
            .collect();
        (params, format!("{name} {{ {} }}", names.join(", ")))
    }

    /// Statements that decode a reply body into `result`.
    fn decode_result(&self, resp: &str, error: Option<&Type>) -> String {
        let rt = self.rt();
        let Some(error) = error else {
            return format!(
                "let at = d.claim({resp}::__SIZE)?;\nlet result = {resp}::__decode(d, at)?;\n"
            );
        };
        let size = self.size(error);
        let dec = self.decode(error, "o");
        format!(
            "let at = d.claim(16)?;\nlet ordinal = d.u64(at)?;\nlet bytes = d.u32(at + 8)?;\nlet handles = d.u32(at + 12)?;\nlet start = d.begin_envelope();\nlet result = match ordinal {{\n1 => {{\nlet o = d.claim({resp}::__SIZE)?;\nOk({resp}::__decode(d, o)?)\n}}\n2 => {{\nlet o = d.claim({size})?;\nErr({dec})\n}}\n_ => return Err({rt}::Error::Invalid),\n}};\nd.end_envelope(start, bytes, handles)?;\n"
        )
    }

    /// The inline size of a reply body and the statements that encode
    /// `result` into it through `e` at `at`.
    fn encode_result(&self, resp: &str, error: Option<&Type>) -> (String, String) {
        let Some(error) = error else {
            return (
                format!("{resp}::__SIZE"),
                "result.__encode(e, at)?;\n".into(),
            );
        };
        let size = self.size(error);
        let enc = self.encode(error, "error", "o");
        (
            "16".into(),
            format!(
                "let start = e.begin_envelope();\nmatch result {{\nOk(results) => {{\ne.put(at, &1u64.to_le_bytes());\nlet o = e.alloc({resp}::__SIZE);\nresults.__encode(e, o)?;\n}}\nErr(error) => {{\ne.put(at, &2u64.to_le_bytes());\nlet o = e.alloc({size});\n{enc}}}\n}}\ne.end_envelope(at + 8, start);\n"
            ),
        )
    }
}
