#![cfg_attr(not(test), no_std)]
#![allow(incomplete_features)]
#![feature(maybe_uninit_uninit_array)]
#![feature(generic_const_exprs)]
#![feature(generic_arg_infer)]
#![feature(fn_traits)]

use core::slice;

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum PinnedStaticVecError {
    CapacityExceeded,
}

#[derive(Debug)]
pub struct PinnedStaticVec<T, const N: usize> {
    len: usize,
    data: [Option<T>; N],
}

const fn const_none<T>(_i: usize) -> Option<T> {
    Option::None
}

impl<T, const N: usize> PinnedStaticVec<T, N> {
    pub fn new(len: usize) -> Result<Self, PinnedStaticVecError> {
        if len > N {
            return Err(PinnedStaticVecError::CapacityExceeded);
        }
        Ok(Self {
            data: core::array::from_fn(const_none),
            len,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[T] {
        //safe as we ensure that 0..len elements are initialized
        unsafe { core::mem::transmute::<_, &[T]>(&self.data[..self.len]) }
    }

    pub fn iter(&self) -> slice::Iter<'_, T> {
        //safe as we ensure that 0..len elements are initialized
        unsafe { core::mem::transmute::<_, core::slice::Iter<'_, T>>(self.data[..self.len].iter()) }
    }

    fn find_slot(&self) -> Option<usize> {
        if self.len >= N {
            return None;
        }

        (0..N).find(|&i| self.data[i].is_none())
    }

    pub fn push(&mut self, item: T) -> Result<(), (PinnedStaticVecError, T)> {
        match self.find_slot() {
            Some(slot) => {
                self.data[slot] = Some(item);
                self.len += 1;
                Ok(())
            }
            None => Err((PinnedStaticVecError::CapacityExceeded, item)),
        }
    }

    fn get_mut(&mut self, index: usize) -> Result<&mut Option<T>, PinnedStaticVecError> {
        if index >= N {
            return Err(PinnedStaticVecError::CapacityExceeded);
        }
        Ok(unsafe { self.data.get_unchecked_mut(index) })
    }

    unsafe fn remove_unsafe(&mut self, index: usize) {
        if self.data[index].is_some() {
            self.len -= 1;
        }
        self.data[index] = None;
    }
}

impl<T, const N: usize> PartialEq for PinnedStaticVec<T, N>
where
    T: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        let a = self.as_slice();
        let b = other.as_slice();

        self.len == other.len && (*a == *b)
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a PinnedStaticVec<T, N> {
    type Item = &'a T;

    type IntoIter = slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T, const N: usize> Default for PinnedStaticVec<T, N> {
    fn default() -> Self {
        Self {
            len: 0,
            data: core::array::from_fn(const_none),
        }
    }
}

impl<T, const N: usize> core::ops::Deref for PinnedStaticVec<T, N> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T, const N: usize> core::ops::Index<usize> for PinnedStaticVec<T, N> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        core::ops::Index::index(&**self, index)
    }
}

pub struct SelectVec<'a, T, const N: usize>(pub &'a mut PinnedStaticVec<T, N>);
impl<'a, T, const N: usize> core::future::Future for SelectVec<'a, T, N>
where
    T: core::future::Future,
{
    type Output = Option<T::Output>;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        let self_mut = self.get_mut();
        let mut found = false;
        for i in 0..N {
            let fut = self_mut.0.get_mut(i);
            if let Ok(Some(fut)) = fut {
                found = true;
                let pin = unsafe { core::pin::Pin::new_unchecked(fut) };
                match core::future::Future::poll(pin, cx) {
                    core::task::Poll::Ready(x) => {
                        unsafe { self_mut.0.remove_unsafe(i) };
                        return core::task::Poll::Ready(Some(x));
                    }
                    core::task::Poll::Pending => {}
                }
            }
        }

        if !found {
            return core::task::Poll::Ready(None);
        }

        core::task::Poll::Pending
    }
}

pub struct SelectVecAndFut<'a, T, const N: usize, F>(pub &'a mut PinnedStaticVec<T, N>, pub F);
impl<'a, T, const N: usize, F> core::future::Future for SelectVecAndFut<'a, T, N, F>
where
    T: core::future::Future,
    F: core::future::Future + Unpin,
{
    type Output = either::Either<F::Output, T::Output>;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        let self_mut = self.get_mut();

        let pin = core::pin::pin!(&mut self_mut.1);
        match core::future::Future::poll(pin, cx) {
            core::task::Poll::Ready(x) => return core::task::Poll::Ready(either::Left(x)),
            core::task::Poll::Pending => {}
        };

        for i in 0..N {
            let fut = self_mut.0.get_mut(i);
            if let Ok(Some(fut)) = fut {
                let pin = unsafe { core::pin::Pin::new_unchecked(fut) };
                match core::future::Future::poll(pin, cx) {
                    core::task::Poll::Ready(x) => {
                        unsafe { self_mut.0.remove_unsafe(i) };
                        return core::task::Poll::Ready(either::Right(x));
                    }
                    core::task::Poll::Pending => {}
                }
            }
        }

        core::task::Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use crate::{PinnedStaticVec, SelectVec};

    async fn coro(id: i8, common: &str) -> Result<i32, &str> {
        println!("coro{id}: start");
        let mut j = 0;
        for i in 1..5 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            j = j + i;
            println!("coro{id}({common}): {i}");
        }
        println!("coro{id}: end");
        Ok(j)
    }

    #[tokio::test]
    async fn test_single_coro() -> Result<(), &'static str> {
        println!("start test");
        let mut v = PinnedStaticVec::<_, 4>::default();
        let coro = coro(1, "hello");

        assert_eq!(v.push(coro).ok(), Some(()));
        assert_eq!(v.len(), 1);

        println!("await SelectVec");
        SelectVec(&mut v).await;

        assert_eq!(v.len(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_single_coro_timeout() -> Result<(), &'static str> {
        println!("start test");
        let mut v = PinnedStaticVec::<_, 4>::default();
        let coro = coro(1, "hello");

        let timeout_coro = tokio::time::timeout(Duration::from_secs(2), coro);

        assert_eq!(v.push(timeout_coro).ok(), Some(()));
        assert_eq!(v.len(), 1);

        println!("await SelectVec");
        SelectVec(&mut v).await;

        assert_eq!(v.len(), 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_four_coros() -> Result<(), &'static str> {
        println!("start test");
        let mut v = PinnedStaticVec::<_, 4>::default();
        let common = "hello";
        let coro1 = coro(1, common);
        let coro2 = coro(2, common);
        let coro3 = coro(3, common);
        let coro4 = coro(4, common);

        let timeout_coro1 = tokio::time::timeout(Duration::from_secs(6), coro1);
        let timeout_coro2 = tokio::time::timeout(Duration::from_secs(6), coro2);
        let timeout_coro3 = tokio::time::timeout(Duration::from_secs(2), coro3);
        let timeout_coro4 = tokio::time::timeout(Duration::from_secs(2), coro4);
        assert_eq!(v.push(timeout_coro1).ok(), Some(()));
        assert_eq!(v.push(timeout_coro2).ok(), Some(()));
        assert_eq!(v.push(timeout_coro3).ok(), Some(()));
        assert_eq!(v.push(timeout_coro4).ok(), Some(()));

        assert_eq!(v.len(), 4);

        println!("await SelectVec");
        SelectVec(&mut v).await;

        assert!(v.len() < 4);

        Ok(())
    }

    #[tokio::test]
    async fn test_overflow() -> Result<(), &'static str> {
        println!("start test");
        let mut v = PinnedStaticVec::<_, 4>::default();
        let common = "hello";
        let coro1 = coro(1, common);
        let coro2 = coro(2, common);
        let coro3 = coro(3, common);
        let coro4 = coro(4, common);
        let coro5 = coro(5, common);

        let timeout_coro1 = tokio::time::timeout(Duration::from_secs(6), coro1);
        let timeout_coro2 = tokio::time::timeout(Duration::from_secs(6), coro2);
        let timeout_coro3 = tokio::time::timeout(Duration::from_secs(2), coro3);
        let timeout_coro4 = tokio::time::timeout(Duration::from_secs(2), coro4);
        let timeout_coro5 = tokio::time::timeout(Duration::from_secs(2), coro5);
        assert_eq!(v.push(timeout_coro1).ok(), Some(()));
        assert_eq!(v.push(timeout_coro2).ok(), Some(()));
        assert_eq!(v.push(timeout_coro3).ok(), Some(()));
        assert_eq!(v.push(timeout_coro4).ok(), Some(()));
        assert_eq!(
            v.push(timeout_coro5).err().map(|e| e.0),
            Some(crate::PinnedStaticVecError::CapacityExceeded)
        );

        assert_eq!(v.len(), 4);

        println!("await SelectVec");
        SelectVec(&mut v).await;

        assert!(v.len() < 4);

        Ok(())
    }
}
