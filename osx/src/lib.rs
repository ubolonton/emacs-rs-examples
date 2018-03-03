#![allow(non_snake_case)]

extern crate libc;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate emacs;
#[macro_use]
extern crate objc;
extern crate cocoa;

use std::{str, slice, mem};
use emacs::{Env, CallEnv, Result, Value, IntoLisp};
use objc::runtime::{Object, Class};
use objc::{Encode, Encoding};
use cocoa::foundation::{NSString as NSString0, NSArray};
use cocoa::foundation::NSFastEnumeration;
use cocoa::base::{id, nil};

emacs_plugin_is_GPL_compatible!();
emacs_module_init!(init);

const MODULE: &str = "osx";
lazy_static! {
    static ref MODULE_PREFIX: String = format!("{}/", MODULE);
}

const UTF8_ENCODING: usize = 4;

// /System/Library/Frameworks/Contacts.framework/Headers/CNContact.h
#[link(name = "Contacts", kind = "framework")]
extern {
    pub static CNContactGivenNameKey: id;
    pub static CNContactFamilyNameKey: id;
    pub static CNContactMiddleNameKey: id;
    pub static CNContactEmailAddressesKey: id;
    pub static CNContactSocialProfilesKey: id;
    pub static CNContactBirthdayKey: id;
    pub static CNContactOrganizationNameKey: id;
    pub static CNContactTypeKey: id;
    pub static CNContactInstantMessageAddressesKey: id;
}

macro_rules! class {
    ($name:ident) => {
        Class::get(stringify!($name)).unwrap()
    };
}

macro_rules! classes {
    ($($name:ident),*) => {
        $(let $name = class!($name);)*
    };
}

#[derive(Debug)]
struct NSString {
    raw: id
}

// FIX
unsafe impl Encode for NSString {
    fn encode() -> Encoding {
        unsafe { Encoding::from_str("@\"NSString\"") }
    }
}

impl NSString {
    fn to_str(&self) -> Option<&str> {
        let raw = self.raw;
        let bytes = unsafe {
            let length = msg_send![raw, lengthOfBytesUsingEncoding:UTF8_ENCODING];
            let ptr: *const u8 = msg_send![raw, UTF8String];
            slice::from_raw_parts(ptr, length)
        };
        str::from_utf8(bytes).ok()
    }
}

/// Get and print an objects description
unsafe fn describe(obj: *mut Object) {
    let description: *mut Object = msg_send![obj, description];
    if let Some(desc_str) = to_s(description) {
        println!("Object description: {}", desc_str);
    }
}

/// Convert an NSString to a String
fn to_s<'a>(nsstring_obj: *mut Object) -> Option<&'a str> {
    let bytes = unsafe {
        let length = msg_send![nsstring_obj, lengthOfBytesUsingEncoding:UTF8_ENCODING];
        let utf8_str: *const u8 = msg_send![nsstring_obj, UTF8String];
        slice::from_raw_parts(utf8_str, length)
    };
    str::from_utf8(bytes).ok()
}

fn find_contacts(env: &CallEnv) -> Result<Value> {
    classes! {
        CNContactStore, CNContact
    }
    let search_str: String = env.parse_arg(0)?;
    let mut values: Vec<Value> = vec![];
    unsafe {
        let store: id = msg_send![CNContactStore, alloc];
        let store: id = msg_send![store, init];
        describe(store);

        let name = NSString0::alloc(nil).init_str(&search_str);
        let keys = NSArray::arrayWithObjects(nil, &[
            CNContactGivenNameKey,
            CNContactFamilyNameKey,
            CNContactMiddleNameKey,
            CNContactOrganizationNameKey,
        ]);
        let error: *mut id = mem::uninitialized();
        let predicate: id = msg_send![CNContact, predicateForContactsMatchingName:name];

        let contacts: id = msg_send![store, unifiedContactsMatchingPredicate:predicate keysToFetch:keys error:error];
        for c in contacts.iter() {
            let value = env.list(&[
                env.intern(":given-name")?,
                (*c).get_ivar::<NSString>("_givenName").to_str().into_lisp(env)?,
                env.intern(":family-name")?,
                (*c).get_ivar::<NSString>("_familyName").to_str().into_lisp(env)?,
                env.intern(":middle-name")?,
                (*c).get_ivar::<NSString>("_middleName").to_str().into_lisp(env)?,
                env.intern(":organization")?,
                (*c).get_ivar::<NSString>("_organizationName").to_str().into_lisp(env)?,
            ])?;
            values.push(value);
        }

        msg_send![store, release];
    }
    env.list(&values)
}

fn init(env: &Env) -> Result<Value> {
    emacs_export_functions! {
        env, *MODULE_PREFIX, {
            "find-contacts" => (find_contacts, 1..1),
        }
    }

    env.provide(MODULE)
}
