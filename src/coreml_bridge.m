#import <CoreML/CoreML.h>
#import <Foundation/Foundation.h>

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    void *model;
    size_t batch_size;
    size_t sequence_length;
    size_t input_dim;
} MlaiCoreMlHandle;

static BOOL mlai_macos_at_least(NSInteger major, NSInteger minor, NSInteger patch) {
    NSOperatingSystemVersion version = {
        .majorVersion = major,
        .minorVersion = minor,
        .patchVersion = patch,
    };
    return [NSProcessInfo.processInfo isOperatingSystemAtLeastVersion:version];
}

static void mlai_set_error(char **output, NSString *message) {
    if (output == NULL) {
        return;
    }
    const char *utf8 = message.UTF8String ?: "unknown Core ML error";
    *output = strdup(utf8);
}

void mlai_coreml_free_error(char *message) {
    free(message);
}

int32_t mlai_coreml_hardware_available(void) {
    if (!mlai_macos_at_least(14, 0, 0)) {
        return 0;
    }
    for (id<MLComputeDeviceProtocol> device in MLAllComputeDevices()) {
        if ([device isKindOfClass:MLNeuralEngineComputeDevice.class]) {
            return 1;
        }
    }
    return 0;
}

static NSInteger mlai_count_neural_engine_operations(
    MLComputePlan *plan,
    MLModelStructureProgramBlock *block
) API_AVAILABLE(macos(14.4)) {
    NSInteger count = 0;
    for (MLModelStructureProgramOperation *operation in block.operations) {
        MLComputePlanDeviceUsage *usage =
            [plan computeDeviceUsageForMLProgramOperation:operation];
        if ([usage.preferredComputeDevice isKindOfClass:MLNeuralEngineComputeDevice.class]) {
            count += 1;
        }
        for (MLModelStructureProgramBlock *child in operation.blocks) {
            count += mlai_count_neural_engine_operations(plan, child);
        }
    }
    return count;
}

int64_t mlai_coreml_neural_engine_operation_count(const char *model_path, char **error_output) {
    @autoreleasepool {
        if (!model_path) {
            mlai_set_error(error_output, @"Core ML model path is missing");
            return -1;
        }
        if (!mlai_macos_at_least(14, 4, 0)) {
            mlai_set_error(error_output, @"Core ML compute-plan inspection requires macOS 14.4 or newer");
            return -1;
        }
        NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:model_path]];
        MLModelConfiguration *configuration = [[MLModelConfiguration alloc] init];
        configuration.computeUnits = MLComputeUnitsCPUAndNeuralEngine;
        dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
        __block MLComputePlan *loaded_plan = nil;
        __block NSError *loaded_error = nil;
        [MLComputePlan loadContentsOfURL:url
                          configuration:configuration
                      completionHandler:^(MLComputePlan *plan, NSError *error) {
            loaded_plan = plan;
            loaded_error = error;
            dispatch_semaphore_signal(semaphore);
        }];
        dispatch_semaphore_wait(semaphore, DISPATCH_TIME_FOREVER);
        if (!loaded_plan) {
            mlai_set_error(error_output, loaded_error.localizedDescription ?: @"Core ML compute plan failed");
            return -1;
        }
        MLModelStructureProgram *program = loaded_plan.modelStructure.program;
        if (!program) {
            mlai_set_error(error_output, @"Core ML artifact is not an ML Program model");
            return -1;
        }
        NSInteger count = 0;
        for (MLModelStructureProgramFunction *function in program.functions.allValues) {
            count += mlai_count_neural_engine_operations(loaded_plan, function.block);
        }
        return (int64_t)count;
    }
}

void *mlai_coreml_load(
    const char *model_path,
    size_t batch_size,
    size_t sequence_length,
    size_t input_dim,
    char **error_output
) {
    @autoreleasepool {
        if (!model_path || batch_size == 0 || sequence_length == 0 || input_dim == 0) {
            mlai_set_error(error_output, @"Invalid Core ML model dimensions");
            return NULL;
        }
        NSString *path = [NSString stringWithUTF8String:model_path];
        MLModelConfiguration *configuration = [[MLModelConfiguration alloc] init];
        configuration.computeUnits = MLComputeUnitsCPUAndNeuralEngine;
        NSError *error = nil;
        MLModel *model = [MLModel modelWithContentsOfURL:[NSURL fileURLWithPath:path]
                                          configuration:configuration
                                                  error:&error];
        if (!model) {
            mlai_set_error(error_output, error.localizedDescription ?: @"Unable to load Core ML model");
            return NULL;
        }

        MLFeatureDescription *input = model.modelDescription.inputDescriptionsByName[@"sequence"];
        NSArray<NSNumber *> *shape = input.multiArrayConstraint.shape;
        if (input.type != MLFeatureTypeMultiArray || shape.count != 3 ||
            shape[0].unsignedLongLongValue != batch_size ||
            shape[1].unsignedLongLongValue != sequence_length ||
            shape[2].unsignedLongLongValue != input_dim) {
            mlai_set_error(error_output, @"Core ML model input shape does not match the LSTM runtime");
            return NULL;
        }

        MlaiCoreMlHandle *handle = calloc(1, sizeof(MlaiCoreMlHandle));
        if (!handle) {
            mlai_set_error(error_output, @"Unable to allocate Core ML handle");
            return NULL;
        }
        handle->model = CFBridgingRetain(model);
        handle->batch_size = batch_size;
        handle->sequence_length = sequence_length;
        handle->input_dim = input_dim;
        return handle;
    }
}

int32_t mlai_coreml_predict(
    void *opaque_handle,
    const float *input_values,
    float *output_values,
    char **error_output
) {
    @autoreleasepool {
        MlaiCoreMlHandle *handle = opaque_handle;
        if (!handle || !input_values || !output_values) {
            mlai_set_error(error_output, @"Invalid Core ML prediction arguments");
            return -1;
        }
        NSError *error = nil;
        MLMultiArray *input = [[MLMultiArray alloc]
            initWithShape:@[@(handle->batch_size), @(handle->sequence_length), @(handle->input_dim)]
            dataType:MLMultiArrayDataTypeFloat32
            error:&error];
        if (!input) {
            mlai_set_error(error_output, error.localizedDescription ?: @"Unable to allocate Core ML input");
            return -1;
        }
        size_t input_count = handle->batch_size * handle->sequence_length * handle->input_dim;
        memcpy(input.dataPointer, input_values, input_count * sizeof(float));
        MLDictionaryFeatureProvider *provider = [[MLDictionaryFeatureProvider alloc]
            initWithDictionary:@{@"sequence": [MLFeatureValue featureValueWithMultiArray:input]}
            error:&error];
        if (!provider) {
            mlai_set_error(error_output, error.localizedDescription ?: @"Unable to create Core ML input provider");
            return -1;
        }
        MLModel *model = (__bridge MLModel *)handle->model;
        id<MLFeatureProvider> result = [model predictionFromFeatures:provider error:&error];
        MLMultiArray *scores = [result featureValueForName:@"score"].multiArrayValue;
        if (!result || !scores || scores.count < handle->batch_size) {
            mlai_set_error(error_output, error.localizedDescription ?: @"Core ML prediction returned an invalid score array");
            return -1;
        }
        if (scores.dataType == MLMultiArrayDataTypeFloat32) {
            memcpy(output_values, scores.dataPointer, handle->batch_size * sizeof(float));
        } else {
            for (size_t index = 0; index < handle->batch_size; index++) {
                output_values[index] = scores[index].floatValue;
            }
        }
        return 0;
    }
}

void mlai_coreml_free(void *opaque_handle) {
    MlaiCoreMlHandle *handle = opaque_handle;
    if (!handle) {
        return;
    }
    if (handle->model) {
        CFRelease(handle->model);
    }
    free(handle);
}
