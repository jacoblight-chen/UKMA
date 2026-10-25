#import <Foundation/Foundation.h>

@interface Graph : NSObject
- (int)add:(int)a;
@end

@implementation Graph
- (int)add:(int)a {
    return a + 1;
}
@end
