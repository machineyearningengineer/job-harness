#import <Foundation/Foundation.h>
#import <AppKit/AppKit.h>
#import <UserNotifications/UserNotifications.h>
#include <stdatomic.h>
#include <stdint.h>
#include <string.h>

// 0 = NotDetermined, 1 = Denied, 2 = Authorized, 3 = Unbundled
static _Atomic int32_t g_auth_status = 0;
static NSString *g_clicked_record = nil;
static id g_lock = nil;

static BOOL jh_is_bundled_app(void) {
    return [[NSBundle mainBundle] bundleIdentifier] != nil;
}

static void jh_refresh_status(void) {
    if (!jh_is_bundled_app()) {
        atomic_store(&g_auth_status, 3);
        return;
    }
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    [center getNotificationSettingsWithCompletionHandler:^(UNNotificationSettings *settings) {
        int32_t st = 0;
        switch (settings.authorizationStatus) {
            case UNAuthorizationStatusAuthorized:
            case UNAuthorizationStatusProvisional:
                st = 2;
                break;
            case UNAuthorizationStatusDenied:
                st = 1;
                break;
            case UNAuthorizationStatusNotDetermined:
            default:
                st = 0;
                break;
        }
        atomic_store(&g_auth_status, st);
    }];
}

@interface JHNotificationDelegate : NSObject <UNUserNotificationCenterDelegate>
@end

@implementation JHNotificationDelegate

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
       willPresentNotification:(UNNotification *)notification
         withCompletionHandler:(void (^)(UNNotificationPresentationOptions options))completionHandler {
    (void)center;
    (void)notification;
    if (@available(macOS 11.0, *)) {
        completionHandler(UNNotificationPresentationOptionBanner | UNNotificationPresentationOptionSound);
    } else {
        completionHandler(UNNotificationPresentationOptionSound);
    }
}

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
didReceiveNotificationResponse:(UNNotificationResponse *)response
         withCompletionHandler:(void (^)(void))completionHandler {
    (void)center;
    NSDictionary *info = response.notification.request.content.userInfo;
    NSString *rec = info[@"record_path"];
    if ([rec isKindOfClass:[NSString class]] && rec.length > 0) {
        @synchronized (g_lock) {
            g_clicked_record = [rec copy];
        }
    }
    dispatch_async(dispatch_get_main_queue(), ^{
        [NSApp activateIgnoringOtherApps:YES];
        for (NSWindow *win in [NSApp windows]) {
            if ([win canBecomeMainWindow]) {
                [win makeKeyAndOrderFront:nil];
                break;
            }
        }
    });
    completionHandler();
}

@end

static JHNotificationDelegate *g_delegate = nil;

void jh_notify_init(void) {
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{
        g_lock = [[NSObject alloc] init];
        if (!jh_is_bundled_app()) {
            atomic_store(&g_auth_status, 3);
            return;
        }
        g_delegate = [[JHNotificationDelegate alloc] init];
        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        center.delegate = g_delegate;
        jh_refresh_status();
    });
}

void jh_notify_request_auth(void) {
    jh_notify_init();
    if (!jh_is_bundled_app()) {
        return;
    }
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    UNAuthorizationOptions opts = UNAuthorizationOptionAlert | UNAuthorizationOptionSound | UNAuthorizationOptionBadge;
    [center requestAuthorizationWithOptions:opts completionHandler:^(BOOL granted, NSError * _Nullable error) {
        (void)error;
        atomic_store(&g_auth_status, granted ? 2 : 1);
        jh_refresh_status();
    }];
}

int32_t jh_notify_auth_status(void) {
    jh_notify_init();
    jh_refresh_status();
    return atomic_load(&g_auth_status);
}

void jh_notify_send(
    const char *req_id,
    const char *title,
    const char *subtitle,
    const char *body,
    const char *record_path
) {
    jh_notify_init();
    if (!jh_is_bundled_app()) {
        return;
    }
    NSString *nsId = (req_id && req_id[0]) ? [NSString stringWithUTF8String:req_id] : [[NSUUID UUID] UUIDString];
    NSString *nsTitle = title ? [NSString stringWithUTF8String:title] : @"Job Harness";
    NSString *nsSub = subtitle ? [NSString stringWithUTF8String:subtitle] : @"";
    NSString *nsBody = body ? [NSString stringWithUTF8String:body] : @"";
    NSString *nsRec = (record_path && record_path[0]) ? [NSString stringWithUTF8String:record_path] : nil;

    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    void (^deliver)(void) = ^{
        UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
        content.title = nsTitle;
        if (nsSub.length > 0) {
            content.subtitle = nsSub;
        }
        content.body = nsBody;
        content.sound = [UNNotificationSound defaultSound];
        if (nsRec != nil) {
            content.userInfo = @{@"record_path": nsRec};
        }
        UNNotificationRequest *req = [UNNotificationRequest requestWithIdentifier:nsId content:content trigger:nil];
        [center addNotificationRequest:req withCompletionHandler:nil];
    };

    if (atomic_load(&g_auth_status) == 0) {
        UNAuthorizationOptions opts = UNAuthorizationOptionAlert | UNAuthorizationOptionSound | UNAuthorizationOptionBadge;
        [center requestAuthorizationWithOptions:opts completionHandler:^(BOOL granted, NSError * _Nullable error) {
            (void)error;
            atomic_store(&g_auth_status, granted ? 2 : 1);
            if (granted) {
                deliver();
            }
        }];
    } else if (atomic_load(&g_auth_status) == 2) {
        deliver();
    }
}

int32_t jh_notify_take_clicked_record(char *buf, int32_t cap) {
    if (!g_lock || !buf || cap <= 1) {
        return 0;
    }
    NSString *taken = nil;
    @synchronized (g_lock) {
        taken = g_clicked_record;
        g_clicked_record = nil;
    }
    if (!taken) {
        return 0;
    }
    const char *utf8 = [taken UTF8String];
    if (!utf8) {
        return 0;
    }
    size_t len = strlen(utf8);
    if ((int32_t)len >= cap) {
        len = (size_t)(cap - 1);
    }
    memcpy(buf, utf8, len);
    buf[len] = '\0';
    return (int32_t)len;
}
