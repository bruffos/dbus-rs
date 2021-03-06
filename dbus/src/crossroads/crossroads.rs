use std::collections::BTreeMap;
use std::any::{TypeId, Any};
use std::ffi::{CString, CStr};
use std::fmt;
use crate::{Path as PathName, Interface as IfaceName, Member as MemberName, Signature, Message, MessageType};
use super::info::{IfaceInfo, MethodInfo, PropInfo};
use super::handlers::{Handlers, SyncInfo};
use super::stdimpl::DBusProperties;

// The key is an IfaceName, but if we have that we bump into https://github.com/rust-lang/rust/issues/59732
// so we use CString as a workaround.
#[derive(Default, Debug)]
struct IfaceReg<H: Handlers>(BTreeMap<CString, (TypeId, IfaceInfo<'static, H>)>);

#[derive(Default)]
pub struct PathData<H: Handlers>(Vec<(TypeId, H::Iface)>);

impl PathData<()> {
    pub fn insert<I: Any + 'static + Send + Sync>(&mut self, i: I) {
        let id = TypeId::of::<I>();
        let t = Box::new(i);
        self.0.push((id, t));
    }
}

impl<H: Handlers> fmt::Debug for PathData<H> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "...") }
}

impl<H: Handlers> PathData<H> {
    fn new() -> Self { PathData(vec!()) }
}

#[derive(Debug)]
struct IfacePaths<H: Handlers>(BTreeMap<CString, PathData<H>>);

impl<H: Handlers> Default for IfacePaths<H> {
    fn default() -> Self { IfacePaths(BTreeMap::new()) }
}

struct MsgHeaders<'a> {
    m: MemberName<'a>,
    i: IfaceName<'a>,
    p: PathName<'a>,
}

fn msg_headers(msg: &Message) -> Option<MsgHeaders> {
    if msg.msg_type() != MessageType::MethodCall { return None };
    let p = msg.path()?;
    let i = msg.interface()?;
    let m = msg.member()?;
    Some(MsgHeaders { m, i, p })
}

pub (super) struct MLookup<'a, H: Handlers> {
    pub (super) cr: &'a Crossroads<H>,
    pub (super) data: &'a PathData<H>,
    pub (super) iface: &'a H::Iface,
    pub (super) iinfo: &'a IfaceInfo<'static, H>,
//    pub (super) minfo: Option<&'a MethodInfo<'static, H>>,
//    pub (super) pinfo: Option<&'a PropInfo<'static, H>>,
}

#[derive(Debug)]
pub struct Crossroads<H: Handlers> {
    reg: IfaceReg<H>,
    paths: IfacePaths<H>,
}

impl<H: Handlers> Crossroads<H> {

    pub fn register<I: 'static>(&mut self, info: IfaceInfo<'static, H>) -> Option<IfaceInfo<'static, H>> {
        self.reg.0.insert(info.name.clone().into_cstring(), (TypeId::of::<I>(), info)).map(|x| x.1)
    }
    pub fn insert<N: Into<PathName<'static>>>(&mut self, name: N, data: PathData<H>) {
        self.paths.0.insert(name.into().into_cstring(), data);
    }
    pub fn get_data<N: Into<PathName<'static>>>(&self, name: N) -> Option<&PathData<H>> {
        self.paths.0.get(name.into().as_cstr())
    }

    fn reg_lookup(&self, headers: &MsgHeaders) -> Option<(MLookup<H>, &MethodInfo<'static, H>)> {
       let (typeid, iinfo) = self.reg.0.get(headers.i.as_cstr())?;
       let minfo = iinfo.methods.iter().find(|x| x.name == headers.m)?;
       let data = self.paths.0.get(headers.p.as_cstr())?;
       let (_, iface) = data.0.iter().find(|x| x.0 == *typeid)?;
       Some((MLookup { cr: self, data, iface, iinfo }, minfo))
    }

    pub (super) fn reg_prop_lookup<'a>(&'a self, data: &'a PathData<H>, iname: &CStr, propname: &CStr) ->
    Option<(MLookup<'a, H>, &PropInfo<'static, H>)> {
       let (typeid, iinfo) = self.reg.0.get(iname)?;
       let pinfo = iinfo.props.iter().find(|x| x.name.as_cstr() == propname)?;
       let (_, iface) = data.0.iter().find(|x| x.0 == *typeid)?;
       Some((MLookup { cr: self, data, iface, iinfo}, pinfo))       
    }
}

impl Crossroads<()> {
    pub fn dispatch(&self, msg: &Message) -> Option<Vec<Message>> {
        let headers = msg_headers(msg)?;
        let (lookup, minfo) = self.reg_lookup(&headers)?;
        let handler = &minfo.handler.0;
        let mut si = SyncInfo { cr: lookup.cr, pd: lookup.data };
        let r = (handler)(&**lookup.iface, msg, &mut si);
        Some(r.into_iter().collect())
    }

    pub fn new_sync() -> Self { 
        let mut cr = Crossroads {
            reg: IfaceReg(BTreeMap::new()),
            paths: IfacePaths(BTreeMap::new()),
        };
        DBusProperties::register(&mut cr);
        cr
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_send_sync() {
        fn is_send<T: Send>(_: &T) {}
        fn is_sync<T: Sync>(_: &T) {}
        let c = Crossroads::new_sync();
        dbg!(&c);
        is_send(&c);
        is_sync(&c);
   }


    #[test]
    fn simple() {
        let mut cr = Crossroads::new_sync();

        struct Score(u16);

        let info = IfaceInfo::new("com.example.dbusrs.crossroads.score", 
            vec!(MethodInfo::new_sync("Hello", |x: &Score, msg, _| {
                assert_eq!(x.0, 7u16);
                Some(msg.method_return().append1(format!("Hello, my score is {}!", x.0)))
            })),
            vec!(PropInfo::new_sync_ro("Score", |x: &Score, _, _| {
                assert_eq!(x.0, 7u16);
                Some(x.0)
            })),
            vec!(),
        );
        cr.register::<Score>(info);

        let mut pdata = PathData::new();
        pdata.insert(Score(7u16));
        pdata.insert(DBusProperties);
        cr.insert("/", pdata);

        let mut msg = Message::new_method_call("com.example.dbusrs.crossroads.score", "/", "com.example.dbusrs.crossroads.score", "Hello").unwrap();
        crate::message::message_set_serial(&mut msg, 57);
        let r = cr.dispatch(&msg).unwrap();
        assert_eq!(r.len(), 1);

        let msg = Message::new_method_call("com.example.dbusrs.crossroads.score", "/", "org.freedesktop.DBus.Properties", "Get").unwrap();
        let mut msg = msg.append2("com.example.dbusrs.crossroads.score", "Score");
        crate::message::message_set_serial(&mut msg, 57);
        let r = cr.dispatch(&msg).unwrap();
        assert_eq!(r.len(), 1);
        let z: u16 = r[0].read1().unwrap();
        assert_eq!(z, 7u16);
    }
}
