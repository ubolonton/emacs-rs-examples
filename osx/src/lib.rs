#![allow(non_snake_case)]

#[macro_use]
extern crate objc;
extern crate cocoa;
extern crate emacs;

use cocoa::base::{id, nil};
use cocoa::foundation::NSFastEnumeration;
use cocoa::foundation::{NSArray, NSString as NSString0};
use emacs::{defun, Env, IntoLisp, Result, Value};
use objc::runtime::{Class, Object};
use objc::{Encode, Encoding};
use std::{mem, slice, str};

emacs::plugin_is_GPL_compatible!();

#[emacs::module(name(fn), separator = "/")]
fn osx(_: &Env) -> Result<()> {
    Ok(())
}

const UTF8_ENCODING: usize = 4;

// /System/Library/Frameworks/Contacts.framework/Headers/CNContact.h
#[link(name = "Contacts", kind = "framework")]
extern "C" {
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
    raw: id,
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
            let length = msg_send![raw, lengthOfBytesUsingEncoding: UTF8_ENCODING];
            let ptr: *const u8 = msg_send![raw, UTF8String];
            slice::from_raw_parts(ptr, length)
        };
        str::from_utf8(bytes).ok()
    }
}

/// Get and print an objects description
pub unsafe fn describe(obj: *mut Object) {
    let description: *mut Object = msg_send![obj, description];
    if let Some(desc_str) = to_s(description) {
        println!("Object description: {}", desc_str);
    }
}

/// Convert an NSString to a String
fn to_s<'a>(nsstring_obj: *mut Object) -> Option<&'a str> {
    let bytes = unsafe {
        let length = msg_send![nsstring_obj, lengthOfBytesUsingEncoding: UTF8_ENCODING];
        let utf8_str: *const u8 = msg_send![nsstring_obj, UTF8String];
        slice::from_raw_parts(utf8_str, length)
    };
    str::from_utf8(bytes).ok()
}

#[defun]
fn find_contacts(env: &Env, search_str: String) -> Result<Value> {
    classes! {
        CNContactStore, CNContact
    }
    let mut values: Vec<Value> = vec![];
    unsafe {
        let store: id = msg_send![CNContactStore, alloc];
        let store: id = msg_send![store, init];
        // describe(store);

        let name = NSString0::alloc(nil).init_str(&search_str);
        let keys = NSArray::arrayWithObjects(
            nil,
            &[
                CNContactGivenNameKey,
                CNContactFamilyNameKey,
                CNContactMiddleNameKey,
                CNContactOrganizationNameKey,
            ],
        );
        let error: *mut id = mem::uninitialized();
        let predicate: id = msg_send![CNContact, predicateForContactsMatchingName: name];

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
